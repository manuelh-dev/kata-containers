// Copyright (c) 2023 Ant Group
//
// SPDX-License-Identifier: Apache-2.0
//

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use hypervisor::device::device_manager::DeviceManager;
use kata_sys_util::mount::{get_mount_path, get_mount_type};
use kata_types::mount::DirectVolumeMountInfo;
use nix::sys::{stat, stat::SFlag};
use oci_spec::runtime as oci;
use tokio::sync::RwLock;

use crate::manager_inner::{block_device_node_is_readonly, device_cgroup_access_is_readonly};
use crate::volume::{
    direct_volumes::{
        get_direct_volume_path, rawblock_volume, spdk_volume, vfio_volume, volume_mount_info,
        KATA_DIRECT_VOLUME_TYPE, KATA_OCI_BLOCK_VOLUME_TYPE, KATA_SPDK_VOLUME_TYPE,
        KATA_SPOOL_VOLUME_TYPE, KATA_VFIO_VOLUME_TYPE,
    },
    utils::KATA_MOUNT_BIND_TYPE,
    Volume,
};

enum DirectVolumeType {
    RawBlock,
    OciBlock,
    Spdk,
    Vfio,
}

fn to_volume_type(volume_type: &str) -> DirectVolumeType {
    match volume_type {
        KATA_OCI_BLOCK_VOLUME_TYPE => DirectVolumeType::OciBlock,
        KATA_SPDK_VOLUME_TYPE | KATA_SPOOL_VOLUME_TYPE => DirectVolumeType::Spdk,
        KATA_VFIO_VOLUME_TYPE => DirectVolumeType::Vfio,
        _ => DirectVolumeType::RawBlock,
    }
}

pub(crate) async fn handle_direct_volume(
    d: &RwLock<DeviceManager>,
    m: &oci::Mount,
    read_only: bool,
    sid: &str,
    linux: Option<&oci::Linux>,
) -> Result<Option<Arc<dyn Volume>>> {
    // In the direct volume scenario, we check if the source of a mount is in the
    // /run/kata-containers/shared/direct-volumes/SID path by iterating over all the mounts.
    // If the source is not in the path with error kind *NotFound*, we ignore the error
    // and we treat it as block volume with oci Mount.type *bind*. Just fill in the block
    // volume info in the DirectVolumeMountInfo
    let mount_info: DirectVolumeMountInfo = match volume_mount_info(&get_mount_path(m.source())) {
        Ok(mount_info) => mount_info,
        Err(e) => {
            // First, We need filter the non-io::ErrorKind.
            if !e.is::<std::io::ErrorKind>() {
                return Err(anyhow!(format!(
                    "unexpected error occurs when parse mount info for {:?}, with error {:?}",
                    m.source(),
                    e.to_string()
                )));
            }

            // Second, we need filter non-NotFound error.
            // Safe to unwrap here, as the error is of type std::io::ErrorKind.
            let error_kind = e.downcast_ref::<std::io::ErrorKind>().unwrap();
            if *error_kind != std::io::ErrorKind::NotFound {
                return Err(anyhow!(format!(
                    "failed to parse volume mount info for {:?}, with error {:?}",
                    m.source(),
                    e.to_string()
                )));
            }

            // Third, if the case is *NotFound* , we just return Ok(None).
            return Ok(None);
        }
    };

    let direct_volume: Arc<dyn Volume> = match to_volume_type(mount_info.volume_type.as_str()) {
        DirectVolumeType::RawBlock => Arc::new(
            rawblock_volume::RawblockVolume::new(d, m, &mount_info, read_only, sid)
                .await
                .with_context(|| format!("new sid {:?} rawblock volume {:?}", sid, m))?,
        ),
        DirectVolumeType::OciBlock => {
            let linux = linux.context("OCI spec has no Linux configuration")?;
            let device = find_oci_block_device(Some(linux), &mount_info.device)?;
            let read_only = read_only
                || mount_info.options.iter().any(|option| option == "ro")
                || device_cgroup_access_is_readonly(
                    linux,
                    oci::LinuxDeviceType::B,
                    device.major(),
                    device.minor(),
                )
                || block_device_node_is_readonly(device.major(), device.minor());
            Arc::new(
                rawblock_volume::RawblockVolume::new_from_oci_device(
                    d,
                    m,
                    &mount_info,
                    &device,
                    read_only,
                    sid,
                )
                .await
                .with_context(|| format!("new sid {:?} OCI block volume {:?}", sid, m))?,
            )
        }
        DirectVolumeType::Spdk => Arc::new(
            spdk_volume::SPDKVolume::new(d, m, &mount_info, read_only, sid)
                .await
                .with_context(|| format!("create spdk volume {m:?}"))?,
        ),
        DirectVolumeType::Vfio => Arc::new(
            vfio_volume::VfioVolume::new(d, m, &mount_info, read_only, sid)
                .await
                .with_context(|| format!("new vfio volume {m:?}"))?,
        ),
    };

    Ok(Some(direct_volume))
}

fn find_oci_block_device(linux: Option<&oci::Linux>, path: &str) -> Result<oci::LinuxDevice> {
    let devices = linux
        .and_then(|linux| linux.devices().as_ref())
        .ok_or_else(|| anyhow!("OCI spec has no Linux devices"))?;
    let matching: Vec<_> = devices
        .iter()
        .filter(|device| {
            device.typ() == oci::LinuxDeviceType::B && device.path().to_string_lossy() == path
        })
        .cloned()
        .collect();

    if matching.len() != 1 {
        return Err(anyhow!(
            "expected exactly one OCI block device at {path:?}, found {}",
            matching.len()
        ));
    }

    Ok(matching[0].clone())
}

pub(crate) fn is_direct_volume(m: &oci::Mount) -> Result<bool> {
    let mnt_type = get_mount_type(m);
    let mount_type = mnt_type.as_str();

    // Filter the non-bind volume and non-direct-vol volume
    let vol_types = [
        KATA_MOUNT_BIND_TYPE,
        KATA_DIRECT_VOLUME_TYPE,
        KATA_VFIO_VOLUME_TYPE,
        KATA_SPDK_VOLUME_TYPE,
        KATA_SPOOL_VOLUME_TYPE,
    ];
    if !vol_types.contains(&mount_type) {
        return Ok(false);
    }

    match get_direct_volume_path(get_mount_path(m.source()).as_str()) {
        Ok(directvol_path) => {
            let fstat = stat::stat(directvol_path.as_str())
                .context(format!("stat mount source {directvol_path} failed."))?;
            Ok(SFlag::from_bits_truncate(fstat.st_mode) == SFlag::S_IFDIR)
        }
        Err(_) => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use oci_spec::runtime::{LinuxBuilder, LinuxDeviceBuilder, LinuxDeviceType};

    fn linux_with_device(path: &str, typ: LinuxDeviceType) -> oci::Linux {
        LinuxBuilder::default()
            .devices(vec![LinuxDeviceBuilder::default()
                .path(PathBuf::from(path))
                .typ(typ)
                .major(8_i64)
                .minor(1_i64)
                .build()
                .unwrap()])
            .build()
            .unwrap()
    }

    #[test]
    fn finds_oci_block_device_by_container_path() {
        let linux = linux_with_device("/dev/kata-volume", LinuxDeviceType::B);

        let device = find_oci_block_device(Some(&linux), "/dev/kata-volume").unwrap();

        assert!(matches!(
            to_volume_type(KATA_OCI_BLOCK_VOLUME_TYPE),
            DirectVolumeType::OciBlock
        ));
        assert_eq!(device.major(), 8);
        assert_eq!(device.minor(), 1);
    }

    #[test]
    fn rejects_missing_oci_block_device() {
        let linux = linux_with_device("/dev/other", LinuxDeviceType::B);

        let err = find_oci_block_device(Some(&linux), "/dev/kata-volume").unwrap_err();

        assert!(err.to_string().contains("found 0"));
    }

    #[test]
    fn rejects_non_block_oci_device() {
        let linux = linux_with_device("/dev/kata-volume", LinuxDeviceType::C);

        let err = find_oci_block_device(Some(&linux), "/dev/kata-volume").unwrap_err();

        assert!(err.to_string().contains("found 0"));
    }

    #[test]
    fn rejects_duplicate_oci_block_device() {
        let device = LinuxDeviceBuilder::default()
            .path(PathBuf::from("/dev/kata-volume"))
            .typ(LinuxDeviceType::B)
            .major(8_i64)
            .minor(1_i64)
            .build()
            .unwrap();
        let linux = LinuxBuilder::default()
            .devices(vec![device.clone(), device])
            .build()
            .unwrap();

        let err = find_oci_block_device(Some(&linux), "/dev/kata-volume").unwrap_err();

        assert!(err.to_string().contains("found 2"));
    }
}
