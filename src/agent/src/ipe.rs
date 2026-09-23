// Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
//
// SPDX-License-Identifier: Apache-2.0

//! Experimental IPE bootstrap for ephemeral Kata guests.
//!
//! This deliberately keeps a throw-away policy signing key in the guest. It is
//! only safe together with the experimental, irreversible IPE seal interface.

use std::collections::BTreeSet;
#[cfg(feature = "devicemapper")]
use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
#[cfg(feature = "devicemapper")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "devicemapper")]
use std::sync::{Condvar, LazyLock, Mutex};

use anyhow::{anyhow, bail, Context, Result};
use nix::mount::{mount, MsFlags};
#[cfg(feature = "devicemapper")]
use openssl::asn1::{Asn1Integer, Asn1Time};
#[cfg(feature = "devicemapper")]
use openssl::bn::BigNum;
#[cfg(feature = "devicemapper")]
use openssl::hash::MessageDigest;
use openssl::pkcs7::{Pkcs7, Pkcs7Flags};
use openssl::pkey::PKey;
#[cfg(feature = "devicemapper")]
use openssl::pkey::Private;
#[cfg(feature = "devicemapper")]
use openssl::rsa::Rsa;
use openssl::stack::Stack;
#[cfg(feature = "devicemapper")]
use openssl::x509::extension::{
    AuthorityKeyIdentifier, BasicConstraints, KeyUsage, SubjectKeyIdentifier,
};
#[cfg(feature = "devicemapper")]
use openssl::x509::X509NameBuilder;
use openssl::x509::X509;
use slog::Logger;

const IPE_SECURITYFS: &str = "/sys/kernel/security";
const IPE_ROOT: &str = "/sys/kernel/security/ipe";
const IPE_POLICY_NAME: &str = "kata_verity";
const IPE_PRIVATE_KEY: &str = "/etc/kata-containers/ipe-prototype/private-key.pem";
const IPE_CERTIFICATE: &str = "/etc/kata-containers/ipe-prototype/certificate.pem";
#[cfg(feature = "devicemapper")]
const DM_VERITY_KEYRING_NAME: &str = ".dm-verity";
#[cfg(feature = "devicemapper")]
const LAYER_CERTIFICATE_DESCRIPTION: &str = "kata-erofs-ephemeral";

#[cfg(feature = "devicemapper")]
const KEYCTL_UNLINK: libc::c_long = 9;
#[cfg(feature = "devicemapper")]
const KEYCTL_RESTRICT_KEYRING: libc::c_long = 29;

#[cfg(feature = "devicemapper")]
static LAYER_SIGNATURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "devicemapper")]
static LAYER_SIGNER: LazyLock<LayerSignerGlobal> = LazyLock::new(LayerSignerGlobal::default);

pub type VerityRootHash = (String, String);

#[cfg(feature = "devicemapper")]
#[derive(Default)]
struct LayerSignerState {
    signer: Option<LayerSigner>,
    finalized: bool,
    active_signatures: usize,
}

#[cfg(feature = "devicemapper")]
#[derive(Default)]
struct LayerSignerGlobal {
    state: Mutex<LayerSignerState>,
    idle: Condvar,
}

#[cfg(feature = "devicemapper")]
struct LayerSigner {
    private_key: PKey<Private>,
    certificate: X509,
}

#[cfg(feature = "devicemapper")]
/// Keeps a detached signature reachable until dm-verity has copied it while
/// loading the target table.
pub(crate) struct RootHashSignature {
    key_description: String,
    signature: Vec<u8>,
}

#[cfg(feature = "devicemapper")]
impl RootHashSignature {
    pub(crate) fn key_description(&self) -> &str {
        &self.key_description
    }

    pub(crate) fn payload(&self) -> &[u8] {
        &self.signature
    }
}

#[cfg(feature = "devicemapper")]
impl Drop for RootHashSignature {
    fn drop(&mut self) {
        if let Ok(mut state) = LAYER_SIGNER.state.lock() {
            state.active_signatures = state.active_signatures.saturating_sub(1);
            if state.active_signatures == 0 {
                LAYER_SIGNER.idle.notify_all();
            }
        }
    }
}

#[cfg(feature = "devicemapper")]
fn add_key(
    key_type: &str,
    description: &str,
    payload: &[u8],
    keyring: libc::c_long,
) -> Result<libc::c_long> {
    let key_type = CString::new(key_type).context("key type contains NUL")?;
    let description = CString::new(description).context("key description contains NUL")?;
    let rc = unsafe {
        libc::syscall(
            libc::SYS_add_key,
            key_type.as_ptr(),
            description.as_ptr(),
            payload.as_ptr(),
            payload.len(),
            keyring,
        )
    };
    if rc == -1 {
        return Err(std::io::Error::last_os_error()).context("add key to kernel keyring");
    }
    Ok(rc)
}

#[cfg(feature = "devicemapper")]
fn unlink_key(key_serial: libc::c_long, keyring: libc::c_long) {
    unsafe {
        libc::syscall(libc::SYS_keyctl, KEYCTL_UNLINK, key_serial, keyring, 0, 0);
    }
}

#[cfg(feature = "devicemapper")]
fn parse_dm_verity_keyring_id(keys: &str) -> Option<libc::c_long> {
    keys.lines().find_map(|line| {
        let fields: Vec<_> = line.split_ascii_whitespace().collect();
        // /proc/keys appends a colon and the key count to keyring
        // descriptions, for example: "keyring .dm-verity: empty".
        if fields.get(7) != Some(&"keyring")
            || fields.get(8)?.trim_end_matches(':') != DM_VERITY_KEYRING_NAME
        {
            return None;
        }
        u32::from_str_radix(fields.first()?, 16)
            .ok()
            .map(|serial| serial as libc::c_long)
    })
}

#[cfg(feature = "devicemapper")]
fn find_dm_verity_keyring() -> Result<libc::c_long> {
    let keys = fs::read_to_string("/proc/keys").context("read /proc/keys")?;
    parse_dm_verity_keyring_id(&keys).ok_or_else(|| {
        anyhow!(
            "{DM_VERITY_KEYRING_NAME} is unavailable; use the experimental dm-verity keyring kernel"
        )
    })
}

#[cfg(feature = "devicemapper")]
fn seal_keyring(keyring: libc::c_long) -> Result<()> {
    let rc = unsafe {
        libc::syscall(
            libc::SYS_keyctl,
            KEYCTL_RESTRICT_KEYRING,
            keyring,
            std::ptr::null::<libc::c_char>(),
            std::ptr::null::<libc::c_char>(),
            0,
        )
    };
    if rc == -1 {
        return Err(std::io::Error::last_os_error())
            .context("seal and activate the dm-verity keyring");
    }
    Ok(())
}

#[cfg(feature = "devicemapper")]
fn generate_layer_signer() -> Result<LayerSigner> {
    let rsa = Rsa::generate(2048).context("generate ephemeral EROFS signing key")?;
    let private_key = PKey::from_rsa(rsa).context("create ephemeral EROFS signing key")?;

    let mut name = X509NameBuilder::new().context("create layer certificate subject")?;
    name.append_entry_by_text("CN", "Kata ephemeral EROFS signer")
        .context("set layer certificate subject")?;
    let name = name.build();

    let mut certificate = X509::builder().context("create layer certificate")?;
    certificate
        .set_version(2)
        .context("set certificate version")?;
    let serial_number = BigNum::from_u32(1).context("create serial number")?;
    let serial = Asn1Integer::from_bn(&serial_number).context("encode serial number")?;
    certificate
        .set_serial_number(&serial)
        .context("set certificate serial number")?;
    certificate
        .set_subject_name(&name)
        .context("set certificate subject")?;
    certificate
        .set_issuer_name(&name)
        .context("set certificate issuer")?;
    certificate
        .set_pubkey(&private_key)
        .context("set certificate public key")?;
    let not_before = Asn1Time::days_from_now(0).context("set certificate start time")?;
    let not_after = Asn1Time::days_from_now(1).context("set certificate expiry")?;
    certificate
        .set_not_before(&not_before)
        .context("apply certificate start time")?;
    certificate
        .set_not_after(&not_after)
        .context("apply certificate expiry")?;
    certificate
        .append_extension(
            BasicConstraints::new()
                .critical()
                .build()
                .context("build certificate constraints")?,
        )
        .context("add certificate constraints")?;
    certificate
        .append_extension(
            KeyUsage::new()
                .critical()
                .digital_signature()
                .build()
                .context("build certificate key usage")?,
        )
        .context("add certificate key usage")?;
    let (subject_key_id, authority_key_id) = {
        let context = certificate.x509v3_context(None, None);
        (
            SubjectKeyIdentifier::new()
                .build(&context)
                .context("build subject key identifier")?,
            AuthorityKeyIdentifier::new()
                .keyid(false)
                .build(&context)
                .context("build authority key identifier")?,
        )
    };
    certificate
        .append_extension(subject_key_id)
        .context("add subject key identifier")?;
    certificate
        .append_extension(authority_key_id)
        .context("add authority key identifier")?;
    certificate
        .sign(&private_key, MessageDigest::sha256())
        .context("self-sign layer certificate")?;
    let certificate = certificate.build();

    Ok(LayerSigner {
        private_key,
        certificate,
    })
}

#[cfg(feature = "devicemapper")]
fn new_layer_signer() -> Result<LayerSigner> {
    let signer = generate_layer_signer()?;
    let keyring = find_dm_verity_keyring()?;
    let certificate_der = signer
        .certificate
        .to_der()
        .context("encode ephemeral EROFS certificate")?;
    let certificate_key = add_key(
        "asymmetric",
        LAYER_CERTIFICATE_DESCRIPTION,
        &certificate_der,
        keyring,
    )
    .context("provision ephemeral EROFS certificate")?;
    if let Err(err) = seal_keyring(keyring) {
        unlink_key(certificate_key, keyring);
        return Err(err);
    }

    info!(
        slog_scope::logger(),
        "provisioned and sealed ephemeral dm-verity layer keyring"
    );
    Ok(signer)
}

#[cfg(feature = "devicemapper")]
impl LayerSigner {
    fn sign(&self, root_hash: &str) -> Result<Vec<u8>> {
        if !is_hex_digest(root_hash) {
            bail!("refusing to sign an invalid dm-verity root hash");
        }

        let certificates = Stack::new().context("create layer certificate stack")?;
        let flags = Pkcs7Flags::BINARY
            | Pkcs7Flags::DETACHED
            | Pkcs7Flags::NOCERTS
            | Pkcs7Flags::NOATTR
            | Pkcs7Flags::NOSMIMECAP;
        let signed = Pkcs7::sign(
            &self.certificate,
            &self.private_key,
            &certificates,
            root_hash.as_bytes(),
            flags,
        )
        .context("sign EROFS dm-verity root hash")?;
        signed
            .to_der()
            .context("encode EROFS dm-verity root hash signature")
    }
}

#[cfg(feature = "devicemapper")]
pub(crate) fn sign_layer_root_hash(root_hash: &str) -> Result<RootHashSignature> {
    let mut state = LAYER_SIGNER
        .state
        .lock()
        .map_err(|_| anyhow!("ephemeral EROFS signer lock is poisoned"))?;
    if state.finalized {
        bail!("ephemeral EROFS signer was destroyed before this layer was attached");
    }
    if state.signer.is_none() {
        state.signer = Some(new_layer_signer()?);
    }

    let signature = state
        .signer
        .as_ref()
        .expect("layer signer was initialized")
        .sign(root_hash)?;
    let sequence = LAYER_SIGNATURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let description = format!("kata-erofs-{}-{sequence}", std::process::id());
    state.active_signatures += 1;

    Ok(RootHashSignature {
        key_description: description,
        signature,
    })
}

#[cfg(feature = "devicemapper")]
/// Prevent further layer authorization and drop the only copy of the
/// ephemeral private key before an untrusted workload can execute.
pub(crate) async fn finalize_layer_signer() -> Result<()> {
    tokio::task::spawn_blocking(|| -> Result<()> {
        let mut state = LAYER_SIGNER
            .state
            .lock()
            .map_err(|_| anyhow!("ephemeral EROFS signer lock is poisoned"))?;
        // Even an image with no EROFS layers must not leave `.dm-verity`
        // writable when an untrusted workload starts. Initializing the signer
        // provisions its certificate and seals the keyring; the private half
        // is then immediately discarded below.
        if state.signer.is_none() {
            state.signer = Some(new_layer_signer()?);
        }
        state.finalized = true;
        while state.active_signatures != 0 {
            state = LAYER_SIGNER
                .idle
                .wait(state)
                .map_err(|_| anyhow!("ephemeral EROFS signer lock is poisoned"))?;
        }
        state.signer.take();
        Ok(())
    })
    .await
    .context("join EROFS signer finalization task")??;
    info!(
        slog_scope::logger(),
        "destroyed ephemeral EROFS layer signing key"
    );
    Ok(())
}

fn is_hex_digest(value: &str) -> bool {
    !value.is_empty()
        && value.len().is_multiple_of(2)
        && value.bytes().all(|c| c.is_ascii_hexdigit())
}

fn is_supported_digest_algorithm(value: &str) -> bool {
    matches!(
        value,
        "blake2b-512"
            | "blake2s-256"
            | "sha256"
            | "sha384"
            | "sha512"
            | "sha3-224"
            | "sha3-256"
            | "sha3-384"
            | "sha3-512"
            | "sm3"
            | "rmd160"
    )
}

fn add_hash(hashes: &mut BTreeSet<VerityRootHash>, algorithm: &str, digest: &str) {
    let algorithm = algorithm.to_ascii_lowercase();
    let digest = digest
        .trim_matches(|c: char| c == '"' || c == '\'' || c == ',')
        .to_ascii_lowercase();

    if is_supported_digest_algorithm(&algorithm) && is_hex_digest(&digest) {
        hashes.insert((algorithm, digest));
    }
}

/// Extract the rootfs `dm-mod.create` hash and extension `root_hash=` values.
/// Kata currently creates all of these images with SHA-256.
pub(crate) fn hashes_from_kernel_cmdline(cmdline: &str) -> BTreeSet<VerityRootHash> {
    let mut hashes = BTreeSet::new();

    for entry in cmdline.match_indices("root_hash=") {
        let value = &cmdline[entry.0 + "root_hash=".len()..];
        let digest = value
            .split(|c: char| c == ',' || c.is_ascii_whitespace() || c == '"')
            .next()
            .unwrap_or_default();
        add_hash(&mut hashes, "sha256", digest);
    }

    // dm-verity target syntax after "verity" is:
    // version data_dev hash_dev data_bs hash_bs data_blocks hash_start alg root_hash salt
    let tokens: Vec<&str> = cmdline.split_ascii_whitespace().collect();
    for (index, token) in tokens.iter().enumerate() {
        if token.trim_matches('"') != "verity" || index + 9 >= tokens.len() {
            continue;
        }
        add_hash(&mut hashes, tokens[index + 8], tokens[index + 9]);
    }

    hashes
}

pub(crate) fn render_policy(hashes: &BTreeSet<VerityRootHash>) -> Result<String> {
    if hashes.is_empty() {
        bail!("refusing to install an IPE policy without a dm-verity root hash");
    }

    let mut policy = String::from(
        "policy_name=kata_verity policy_version=0.0.0\n\
         DEFAULT action=ALLOW\n\
         DEFAULT op=EXECUTE action=DENY\n",
    );

    for (algorithm, digest) in hashes {
        if !is_supported_digest_algorithm(algorithm) || !is_hex_digest(digest) {
            bail!("invalid dm-verity digest or algorithm in IPE policy");
        }
        policy.push_str(&format!(
            "op=EXECUTE dmverity_roothash={algorithm}:{digest} action=ALLOW\n"
        ));
    }
    policy.push_str("op=EXECUTE dmverity_signature=TRUE action=ALLOW\n");

    Ok(policy)
}

fn sign_policy(policy: &[u8]) -> Result<Vec<u8>> {
    let key_data = fs::read(IPE_PRIVATE_KEY)
        .with_context(|| format!("read prototype IPE key {IPE_PRIVATE_KEY}"))?;
    let cert_data = fs::read(IPE_CERTIFICATE)
        .with_context(|| format!("read prototype IPE certificate {IPE_CERTIFICATE}"))?;

    let key = PKey::private_key_from_pem(&key_data).context("parse prototype IPE private key")?;
    let cert = X509::from_pem(&cert_data)
        .or_else(|_| X509::from_der(&cert_data))
        .context("parse prototype IPE certificate")?;
    let certificates = Stack::new().context("create PKCS#7 certificate stack")?;
    let flags = Pkcs7Flags::BINARY | Pkcs7Flags::NOATTR | Pkcs7Flags::NOSMIMECAP;
    let signed = Pkcs7::sign(&cert, &key, &certificates, policy, flags)
        .context("sign prototype IPE policy")?;

    signed.to_der().context("encode prototype IPE policy")
}

fn write_securityfs(path: &str, data: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .with_context(|| format!("open {path}"))?;
    file.write_all(data)
        .with_context(|| format!("write {path}"))
}

fn ensure_securityfs() -> Result<()> {
    if Path::new(&format!("{IPE_ROOT}/new_policy")).exists() {
        return Ok(());
    }

    fs::create_dir_all(IPE_SECURITYFS).context("create securityfs mount point")?;
    mount(
        Some("securityfs"),
        IPE_SECURITYFS,
        Some("securityfs"),
        MsFlags::MS_NOSUID | MsFlags::MS_NODEV | MsFlags::MS_NOEXEC,
        None::<&str>,
    )
    .context("mount securityfs")?;

    if !Path::new(&format!("{IPE_ROOT}/new_policy")).exists() {
        bail!("IPE securityfs interface is unavailable");
    }
    Ok(())
}

/// Install, activate, enforce, and irreversibly seal the generated policy.
pub(crate) fn activate_and_seal(logger: &Logger) -> Result<()> {
    let cmdline = fs::read_to_string("/proc/cmdline").context("read kernel command line")?;
    let hashes = hashes_from_kernel_cmdline(&cmdline);

    let policy = render_policy(&hashes)?;
    let signed_policy = sign_policy(policy.as_bytes())?;

    ensure_securityfs()?;
    write_securityfs(&format!("{IPE_ROOT}/new_policy"), &signed_policy)
        .context("deploy prototype IPE policy")?;
    write_securityfs(
        &format!("{IPE_ROOT}/policies/{IPE_POLICY_NAME}/active"),
        b"1",
    )
    .context("activate prototype IPE policy")?;
    write_securityfs(&format!("{IPE_ROOT}/enforce"), b"1").context("enable IPE enforcement")?;

    let seal = format!("{IPE_ROOT}/seal");
    if !Path::new(&seal).exists() {
        return Err(anyhow!(
            "IPE seal interface is unavailable; use the ipe-experimental Kata kernel"
        ));
    }
    write_securityfs(&seal, b"1").context("irreversibly seal IPE")?;

    info!(
        logger,
        "activated and sealed prototype IPE policy";
        "dm-verity-root-hashes" => hashes.len(),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rootfs_and_extension_hashes() {
        let root = "aa".repeat(32);
        let extension = "bb".repeat(32);
        let cmdline = format!(
            "dm-mod.create=\"dm-verity,,,ro,0 8 verity 1 /dev/vda1 /dev/vda2 4096 4096 1 0 sha256 {root} -\" kata.extension.gpu.verity_params=root_hash={extension},salt=-"
        );
        let hashes = hashes_from_kernel_cmdline(&cmdline);

        assert!(hashes.contains(&("sha256".to_string(), root)));
        assert!(hashes.contains(&("sha256".to_string(), extension)));
    }

    #[test]
    fn policy_only_denies_untrusted_execution() {
        let mut hashes = BTreeSet::new();
        hashes.insert(("sha256".to_string(), "ab".repeat(32)));
        let policy = render_policy(&hashes).unwrap();

        assert!(policy.contains("DEFAULT action=ALLOW"));
        assert!(policy.contains("DEFAULT op=EXECUTE action=DENY"));
        assert!(policy.contains("dmverity_roothash=sha256:"));
        assert!(policy.contains("dmverity_signature=TRUE"));
    }

    #[test]
    #[cfg(feature = "devicemapper")]
    fn parse_dm_verity_keyring() {
        let keys = "1a2b3c4d I------ 1 perm 3f010000 0 0 keyring .dm-verity: empty\n";
        assert_eq!(parse_dm_verity_keyring_id(keys), Some(0x1a2b3c4d));
    }

    #[test]
    #[cfg(feature = "devicemapper")]
    fn layer_signature_verifies_as_detached_pkcs7() {
        use openssl::x509::store::X509StoreBuilder;

        let signer = generate_layer_signer().unwrap();
        let root_hash = "ab".repeat(32);
        let signature = signer.sign(&root_hash).unwrap();
        let signature = Pkcs7::from_der(&signature).unwrap();
        let mut certificates = Stack::new().unwrap();
        certificates.push(signer.certificate.clone()).unwrap();
        let store = X509StoreBuilder::new().unwrap().build();

        signature
            .verify(
                &certificates,
                &store,
                Some(root_hash.as_bytes()),
                None,
                Pkcs7Flags::BINARY | Pkcs7Flags::NOVERIFY,
            )
            .unwrap();
        assert!(signature
            .verify(
                &certificates,
                &store,
                Some(b"wrong-root-hash"),
                None,
                Pkcs7Flags::BINARY | Pkcs7Flags::NOVERIFY,
            )
            .is_err());
    }
}
