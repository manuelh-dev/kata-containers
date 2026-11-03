// Copyright (c) 2026 NVIDIA Corporation
//
// SPDX-License-Identifier: Apache-2.0
//

use std::collections::HashSet;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

/// Experimental annotation that maps Kubernetes volume devices to guest mounts.
pub const BLOCK_DEVICE_MOUNTS_ANNOTATION: &str =
    "io.katacontainers.experimental.block-device-mounts";

const SUPPORTED_FS_TYPE: &str = "ext4";
const ALLOWED_MOUNT_OPTIONS: &[&str] = &[
    "ro",
    "rw",
    "nosuid",
    "nodev",
    "noexec",
    "noatime",
    "relatime",
    "nodiratime",
    "strictatime",
    "sync",
    "dirsync",
    "discard",
];

/// Payload of the experimental block-device mount annotation.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BlockDeviceMounts {
    /// Requested device-to-mount mappings.
    pub mounts: Vec<BlockDeviceMount>,
}

/// One unambiguous Kubernetes container/volume device-to-mount mapping.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BlockDeviceMount {
    /// Kubernetes container name.
    pub container: String,
    /// Kubernetes volume name referenced by the container's volumeDevice.
    pub volume: String,
    /// volumeDevice path from the Kubernetes container.
    pub device_path: String,
    /// Desired filesystem mount destination inside the container.
    pub mount_path: String,
    /// Filesystem type to mount.
    pub fs_type: String,
    /// Filesystem and bind-mount options.
    #[serde(default)]
    pub mount_options: Vec<String>,
}

/// Parse and validate an experimental block-device mount annotation.
pub fn parse_block_device_mounts(value: &str) -> Result<BlockDeviceMounts, String> {
    let config: BlockDeviceMounts = serde_json::from_str(value)
        .map_err(|e| format!("invalid block-device-mounts annotation JSON: {e}"))?;
    validate_block_device_mounts(&config)?;
    Ok(config)
}

fn validate_block_device_mounts(config: &BlockDeviceMounts) -> Result<(), String> {
    if config.mounts.is_empty() {
        return Err("block-device-mounts annotation contains no mounts".to_string());
    }

    let mut volumes = HashSet::new();
    let mut devices = HashSet::new();
    let mut destinations = HashSet::new();

    for mount in &config.mounts {
        if mount.container.is_empty() {
            return Err("block-device-mount entry has an empty container".to_string());
        }
        if mount.volume.is_empty() {
            return Err("block-device-mount entry has an empty volume".to_string());
        }
        validate_device_path(&mount.device_path)?;
        validate_mount_path(&mount.mount_path)?;

        if mount.fs_type != SUPPORTED_FS_TYPE {
            return Err(format!(
                "block-device-mount fsType {:?} is unsupported; only {SUPPORTED_FS_TYPE} is allowed",
                mount.fs_type
            ));
        }

        let mut options = HashSet::new();
        for option in &mount.mount_options {
            if !ALLOWED_MOUNT_OPTIONS.contains(&option.as_str()) {
                return Err(format!(
                    "block-device-mount option {:?} is unsupported",
                    option
                ));
            }
            if !options.insert(option.as_str()) {
                return Err(format!(
                    "block-device-mount option {:?} is duplicated",
                    option
                ));
            }
        }
        if options.contains("ro") && options.contains("rw") {
            return Err("block-device-mount options cannot contain both ro and rw".to_string());
        }

        if !volumes.insert(&mount.volume) {
            return Err(format!(
                "block-device-mount volume {:?} is mapped more than once",
                mount.volume
            ));
        }
        if !devices.insert((&mount.container, &mount.device_path)) {
            return Err(format!(
                "block-device-mount device {:?} is mapped more than once for container {:?}",
                mount.device_path, mount.container
            ));
        }
        if !destinations.insert((&mount.container, &mount.mount_path)) {
            return Err(format!(
                "block-device-mount destination {:?} is mapped more than once for container {:?}",
                mount.mount_path, mount.container
            ));
        }
    }

    Ok(())
}

fn validate_device_path(path: &str) -> Result<(), String> {
    let path = Path::new(path);
    if !is_clean_absolute_path(path) || path == Path::new("/dev") || !path.starts_with("/dev") {
        return Err(format!(
            "block-device-mount devicePath {:?} must be a clean path below /dev",
            path
        ));
    }
    Ok(())
}

fn validate_mount_path(path: &str) -> Result<(), String> {
    let path = Path::new(path);
    if !is_clean_absolute_path(path) || path == Path::new("/") {
        return Err(format!(
            "block-device-mount mountPath {:?} must be a clean, non-root absolute path",
            path
        ));
    }

    for reserved in ["/dev", "/proc", "/sys", "/run/kata-containers"] {
        if path.starts_with(reserved) {
            return Err(format!(
                "block-device-mount mountPath {:?} is below reserved path {reserved}",
                path
            ));
        }
    }
    Ok(())
}

fn is_clean_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"{
        "mounts": [{
            "container": "app",
            "volume": "data",
            "devicePath": "/dev/kata-data",
            "mountPath": "/var/lib/data",
            "fsType": "ext4",
            "mountOptions": ["nosuid", "nodev"]
        }]
    }"#;

    #[test]
    fn parses_valid_annotation() {
        let config = parse_block_device_mounts(VALID).unwrap();

        assert_eq!(config.mounts[0].container, "app");
        assert_eq!(config.mounts[0].mount_path, "/var/lib/data");
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(parse_block_device_mounts("{").is_err());
    }

    #[test]
    fn rejects_unsafe_destination() {
        let value = VALID.replace("/var/lib/data", "/proc/sysrq-trigger");
        assert!(parse_block_device_mounts(&value)
            .unwrap_err()
            .contains("reserved path"));
    }

    #[test]
    fn rejects_unsupported_fstype() {
        let value = VALID.replace("\"ext4\"", "\"xfs\"");
        assert!(parse_block_device_mounts(&value)
            .unwrap_err()
            .contains("unsupported"));
    }

    #[test]
    fn rejects_unsafe_mount_option() {
        let value = VALID.replace("\"nosuid\", \"nodev\"", "\"suid\"");
        assert!(parse_block_device_mounts(&value)
            .unwrap_err()
            .contains("unsupported"));
    }

    #[test]
    fn rejects_ambiguous_volume_mapping() {
        let value = VALID.replace("}]\n", "}, {\"container\":\"other\",\"volume\":\"data\",\"devicePath\":\"/dev/other\",\"mountPath\":\"/data\",\"fsType\":\"ext4\"}]\n");
        assert!(parse_block_device_mounts(&value)
            .unwrap_err()
            .contains("mapped more than once"));
    }
}
