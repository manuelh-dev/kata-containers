// Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
//
// SPDX-License-Identifier: Apache-2.0

//! Experimental IPE bootstrap for ephemeral Kata guests.
//!
//! This deliberately keeps a throw-away policy signing key in the guest. It is
//! only safe together with the experimental, irreversible IPE seal interface.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use nix::mount::{mount, MsFlags};
use openssl::pkcs7::{Pkcs7, Pkcs7Flags};
use openssl::pkey::PKey;
use openssl::stack::Stack;
use openssl::x509::X509;
use slog::Logger;

const IPE_SECURITYFS: &str = "/sys/kernel/security";
const IPE_ROOT: &str = "/sys/kernel/security/ipe";
const IPE_POLICY_NAME: &str = "kata_verity";
const IPE_PRIVATE_KEY: &str = "/etc/kata-containers/ipe-prototype/private-key.pem";
const IPE_CERTIFICATE: &str = "/etc/kata-containers/ipe-prototype/certificate.pem";

pub type VerityRootHash = (String, String);

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
pub(crate) fn activate_and_seal(
    erofs_hashes: &BTreeSet<VerityRootHash>,
    logger: &Logger,
) -> Result<()> {
    let cmdline = fs::read_to_string("/proc/cmdline").context("read kernel command line")?;
    let mut hashes = hashes_from_kernel_cmdline(&cmdline);
    hashes.extend(erofs_hashes.iter().cloned());

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
    }
}
