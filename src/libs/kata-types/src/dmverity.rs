// Copyright (c) 2026 Ant Group
//
// SPDX-License-Identifier: Apache-2.0
//
//

use anyhow::{anyhow, Context, Result};
use devicemapper::{DevId, DmFlags, DmName, DmOptions, DmUdevFlags, DM};
use nix::sys::stat::{self, Mode, SFlag};
use slog::Logger;
use std::ffi::CString;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::time::sleep;

pub use crate::mount::DmVerityInfo;

/// Detect whether udevd is running in the guest.
///
/// Checks for the udevd control socket — its presence reliably indicates a
/// running udevd. The result is cached for the process lifetime since udev
/// availability does not change after boot.
pub fn has_udev() -> bool {
    static UDEV_AVAILABLE: OnceLock<bool> = OnceLock::new();
    *UDEV_AVAILABLE.get_or_init(|| Path::new("/run/udev/control").exists())
}

/// DmOptions with all udev interactions disabled, for use when udev is not running.
fn no_udev_dm_options() -> DmOptions {
    DmOptions::default().set_udev_flags(
        DmUdevFlags::DM_UDEV_DISABLE_LIBRARY_FALLBACK
            | DmUdevFlags::DM_UDEV_DISABLE_SUBSYSTEM_RULES_FLAG
            | DmUdevFlags::DM_UDEV_DISABLE_DISK_RULES_FLAG
            | DmUdevFlags::DM_UDEV_DISABLE_OTHER_RULES_FLAG
            | DmUdevFlags::DM_UDEV_DISABLE_DM_RULES_FLAG,
    )
}

/// DmOptions for creating a read-only dm-verity device: udev-aware.
fn dm_opts_readonly() -> DmOptions {
    no_udev_dm_options().set_flags(DmFlags::DM_READONLY)
}

/// DmOptions for deferred device removal: udev-aware.
fn dm_opts_deferred_remove() -> DmOptions {
    no_udev_dm_options().set_flags(DmFlags::DM_DEFERRED_REMOVE)
}

/// DmOptions for creating a dm-verity device, with appropriate flags based on udev availability.
#[allow(dead_code)]
fn dm_create_options() -> DmOptions {
    if has_udev() {
        DmOptions::default().set_flags(DmFlags::DM_READONLY)
    } else {
        dm_opts_readonly()
    }
}

/// DmOptions for device suspend/resume: udev-aware.
#[allow(dead_code)]
fn dm_suspend_options() -> DmOptions {
    if has_udev() {
        DmOptions::default()
    } else {
        no_udev_dm_options()
    }
}

/// DmOptions for deferred device removal: udev-aware.
fn dm_remove_options() -> DmOptions {
    if has_udev() {
        DmOptions::default().set_flags(DmFlags::DM_DEFERRED_REMOVE)
    } else {
        dm_opts_deferred_remove()
    }
}

/// Create a block device node for a dm-verity device using mknod(2).
pub fn create_dm_dev_node(name: &str, dev: devicemapper::Device) -> Result<String> {
    let mapper_dir = Path::new("/dev/mapper");
    if !mapper_dir.exists() {
        std::fs::create_dir_all(mapper_dir)
            .with_context(|| format!("failed to create directory {}", mapper_dir.display()))?;
    }

    let dev_path = format!("/dev/mapper/{}", name);
    if Path::new(&dev_path).exists() {
        std::fs::remove_file(&dev_path)
            .with_context(|| format!("failed to remove stale device node {}", dev_path))?;
    }

    let dev_t: nix::libc::dev_t = dev.into();
    stat::mknod(
        dev_path.as_str(),
        SFlag::S_IFBLK,
        Mode::from_bits_truncate(0o600),
        dev_t,
    )
    .with_context(|| format!("failed to mknod block device {}", dev_path))?;

    Ok(dev_path)
}

/// Remove a device node created by `create_dm_dev_node`.
pub fn remove_dm_dev_node(dev_path: &str) {
    if dev_path.starts_with("/dev/mapper/") && Path::new(dev_path).exists() {
        if let Err(e) = std::fs::remove_file(dev_path) {
            slog::warn!(
                slog_scope::logger(),
                "failed to remove dm device node";
                "path" => dev_path,
                "error" => %e,
            );
        }
    }
}

/// Generate a unique dm-verity device name from source path and verity hash.
pub fn build_dmverity_device_name(source_device_path: &Path, verity_info: &DmVerityInfo) -> String {
    let source_short = source_device_path
        .file_name()
        .map(|f| f.to_string_lossy())
        .unwrap_or_default();
    let hash_prefix = &verity_info.hash[..verity_info.hash.len().min(32)];
    let mut name = format!(
        "kata-verity-{}-off{}-{}",
        source_short, verity_info.offset, hash_prefix
    );
    name.truncate(128);
    name
}

/// Result of dm-verity device setup, indicating whether the device node is ready or if we need to wait for udev.
enum DmSetupResult {
    Ready(String),
    NeedUdevWait,
}

const KEY_SPEC_THREAD_KEYRING: nix::libc::c_long = -1;
const KEYCTL_UNLINK: nix::libc::c_long = 9;

struct TemporaryUserKey {
    serial: nix::libc::c_long,
}

impl Drop for TemporaryUserKey {
    fn drop(&mut self) {
        unsafe {
            nix::libc::syscall(
                nix::libc::SYS_keyctl,
                KEYCTL_UNLINK,
                self.serial,
                KEY_SPEC_THREAD_KEYRING,
                0,
                0,
            );
        }
    }
}

fn publish_signature_key(description: &str, payload: &[u8]) -> Result<TemporaryUserKey> {
    let key_type = CString::new("user").expect("static key type has no NUL");
    let description =
        CString::new(description).context("signature key description contains NUL")?;
    let serial = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_add_key,
            key_type.as_ptr(),
            description.as_ptr(),
            payload.as_ptr(),
            payload.len(),
            KEY_SPEC_THREAD_KEYRING,
        )
    };
    if serial == -1 {
        return Err(std::io::Error::last_os_error())
            .context("publish dm-verity signature in ioctl thread keyring");
    }

    Ok(TemporaryUserKey { serial })
}

fn build_verity_params(
    verity_info: &DmVerityInfo,
    source_display: &str,
    hash_start_block: u64,
) -> Result<String> {
    let salt = verity_info.salt.as_deref().unwrap_or("-");
    let mut params = format!(
        "{} {} {} {} {} {} {} {} {} {}",
        verity_info.hash_type,
        source_display,
        source_display,
        verity_info.blocksize,
        verity_info.hashsize,
        verity_info.blocknum,
        hash_start_block,
        verity_info.hashtype,
        verity_info.hash,
        salt
    );

    if let Some(key_desc) = &verity_info.root_hash_sig_key_desc {
        if key_desc.is_empty() || key_desc.bytes().any(|c| c.is_ascii_whitespace()) {
            return Err(anyhow!(
                "invalid dm-verity root hash signature key description"
            ));
        }
        params.push_str(&format!(" 2 root_hash_sig_key_desc {key_desc}"));
    }

    Ok(params)
}

/// Destroy a dm-verity device by name.
pub fn destroy_dmverity_device(verity_device_name: &str) -> Result<()> {
    let dm = devicemapper::DM::new()?;
    let name = devicemapper::DmName::new(verity_device_name)?;

    dm.device_remove(&devicemapper::DevId::Name(name), dm_remove_options())
        .context(format!("remove DmverityDevice {}", verity_device_name))?;

    Ok(())
}

/// Destroy a dm-verity device by its `/dev/mapper/` path.
pub fn destroy_partition_dmverity_device(verity_device_path: &str, logger: &Logger) -> Result<()> {
    // The verity device path is /dev/mapper/<name> (as created by create_dm_dev_node).
    // Extract the DM device name for removal. Also remove the mknod-created device node.
    let device_name = verity_device_path
        .strip_prefix("/dev/mapper/")
        .unwrap_or(verity_device_path)
        .to_string();

    destroy_dmverity_device(&device_name).context("Failed to destroy dm-verity device")?;
    info!(
        logger,
        "Destroying dm-verity device";
        "device-name" => &device_name,
    );

    // Only remove the device node manually if we created it via mknod.
    // When udev is running, it handles node lifecycle automatically.
    if !has_udev() {
        remove_dm_dev_node(verity_device_path);
    }

    Ok(())
}

/// Clean up all dm-verity devices for a multi-layer EROFS mount.
pub fn cleanup_dmverity_devices(verity_devices: &[String], logger: &Logger) {
    info!(
        logger,
        "Cleaning up {} dm-verity devices",
        verity_devices.len()
    );

    // Destroy in reverse order
    for verity_device in verity_devices.iter().rev() {
        if let Err(e) = destroy_partition_dmverity_device(verity_device, logger) {
            warn!(
                logger,
                "Failed to destroy dm-verity device";
                "device-path" => verity_device,
                "error" => format!("{:#}", e),
            );
        }
    }

    info!(logger, "dm-verity device cleanup completed");
}

/// Wait for udev to create a device-mapper node under `/dev/mapper/`.
pub async fn wait_for_dm_dev_node(name: &str) -> Result<String> {
    let dev_path = format!("/dev/mapper/{}", name);
    let path = Path::new(&dev_path);

    if path.exists() {
        return Ok(dev_path);
    }

    const MAX_WAIT_MS: u64 = 2000;
    const POLL_INTERVAL_MS: u64 = 50;

    for _attempt in 0..(MAX_WAIT_MS / POLL_INTERVAL_MS) {
        sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
        if path.exists() {
            return Ok(dev_path);
        }
    }

    Err(anyhow!(
        "udev did not create dm device node {} within {} ms",
        dev_path,
        MAX_WAIT_MS
    ))
}

/// Create a dm-verity device using devicemapper, offloading blocking ioctls to a dedicated thread.
pub async fn create_dmverity_device(
    verity_info: &DmVerityInfo,
    source_device_path: &Path,
) -> Result<String> {
    let verity_info = verity_info.clone();
    let source_path = source_device_path.to_path_buf();

    let verity_name_string = build_dmverity_device_name(&source_path, &verity_info);
    let verity_name_for_wait = verity_name_string.clone();

    // Offload all blocking ioctl operations to a dedicated thread.
    // Always use no-udev DmOptions inside spawn_blocking to avoid DM_UDEV_WAIT
    // blocking on udevd event processing. When udev is running, we wait for the
    // device node asynchronously after the ioctl completes (via wait_for_dm_dev_node).
    let dev_path_or_need_udev = tokio::task::spawn_blocking(move || -> Result<DmSetupResult> {
        let _signature_key = match (
            verity_info.root_hash_sig_key_desc.as_deref(),
            verity_info.root_hash_sig.as_deref(),
        ) {
            (Some(description), Some(payload)) => {
                Some(publish_signature_key(description, payload)?)
            }
            (None, None) => None,
            _ => {
                return Err(anyhow!(
                    "dm-verity signature description and payload must be supplied together"
                ));
            }
        };

        let dm = DM::new()?;
        let verity_name = DmName::new(&verity_name_string)?;
        let id = DevId::Name(verity_name);

        let opts = no_udev_dm_options();
        let ro_opts = dm_opts_readonly();

        // Step 0: Remove stale device if it already exists
        let remove_opts = dm_opts_deferred_remove();
        if dm.device_remove(&id, remove_opts).is_ok() {
            // Stale device removed; continue with creation.
        }

        // Step 1: Create device as read-only
        dm.device_create(verity_name, None, ro_opts)?;

        // Calculate hash start block.
        let hash_start_block: u64 = if verity_info.no_superblock {
            verity_info.offset / verity_info.hashsize
        } else {
            let superblock_blocks = 512_u64.div_ceil(verity_info.hashsize);
            (verity_info.offset / verity_info.hashsize) + superblock_blocks
        };

        let source_display = source_path.display().to_string();
        let verity_params = build_verity_params(&verity_info, &source_display, hash_start_block)?;

        let verity_table = vec![(
            0,
            verity_info.blocknum * verity_info.blocksize / 512,
            "verity".into(),
            verity_params.clone(),
        )];

        info!(
            slog_scope::logger(),
            "dm-verity table parameters";
            "device" => &source_display,
            "data_blocks" => verity_info.blocknum,
            "data_block_size" => verity_info.blocksize,
            "hash_block_size" => verity_info.hashsize,
            "hash_start_block" => hash_start_block,
            "hash_algorithm" => &verity_info.hashtype,
            "hash_type" => verity_info.hash_type,
            "no_superblock" => verity_info.no_superblock,
            "salt" => verity_info.salt.as_deref().unwrap_or("-"),
            "root-hash-signed" => verity_info.root_hash_sig_key_desc.is_some(),
            "table_params" => &verity_params,
        );

        // Step 2: Load table and resume (activate)
        dm.table_load(&id, verity_table.as_slice(), ro_opts)?;
        dm.device_suspend(&id, opts)?;

        // Step 3: Ensure the device node exists under /dev/mapper/.
        let result = if has_udev() {
            DmSetupResult::NeedUdevWait
        } else {
            info!(
                slog_scope::logger(),
                "udev is not running; creating dm-verity device node manually";
                "device-name" => &verity_name_string,
            );
            let device_info = dm.device_info(&id)?;
            let path = create_dm_dev_node(&verity_name_string, device_info.device())?;
            DmSetupResult::Ready(path)
        };

        Ok(result)
    })
    .await
    .context("spawn_blocking for dm-verity ioctl panicked")??;

    // If udev is running, wait asynchronously for the device node (non-blocking poll).
    let dev_path = match dev_path_or_need_udev {
        DmSetupResult::Ready(path) => path,
        DmSetupResult::NeedUdevWait => {
            info!(
                slog_scope::logger(),
                "Waiting for udev to create dm-verity device node";
                "device-name" => &verity_name_for_wait,
            );
            wait_for_dm_dev_node(&verity_name_for_wait).await?
        }
    };

    Ok(dev_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_info() -> DmVerityInfo {
        DmVerityInfo {
            hashtype: "sha256".to_string(),
            hash: "ab".repeat(32),
            blocknum: 8,
            blocksize: 4096,
            hashsize: 4096,
            offset: 32768,
            salt: None,
            hash_type: 1,
            no_superblock: true,
            root_hash_sig_key_desc: None,
            root_hash_sig: None,
        }
    }

    #[test]
    fn verity_params_include_root_hash_signature() {
        let mut info = test_info();
        info.root_hash_sig_key_desc = Some("kata-erofs-42".to_string());
        let params = build_verity_params(&info, "/dev/vda1", 8).unwrap();

        assert!(params.ends_with(" 2 root_hash_sig_key_desc kata-erofs-42"));
    }

    #[test]
    fn verity_params_reject_unsafe_key_description() {
        let mut info = test_info();
        info.root_hash_sig_key_desc = Some("not safe".to_string());

        assert!(build_verity_params(&info, "/dev/vda1", 8).is_err());
    }
}
