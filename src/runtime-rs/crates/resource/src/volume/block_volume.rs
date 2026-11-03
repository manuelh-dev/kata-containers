// Copyright (c) 2019-2022 Alibaba Cloud
// Copyright (c) 2019-2022 Ant Group
//
// SPDX-License-Identifier: Apache-2.0
//

use super::Volume;
use std::path::PathBuf;

use crate::volume::utils::{
    handle_block_volume, is_block_device_readonly, DEFAULT_VOLUME_FS_TYPE, KATA_MOUNT_BIND_TYPE,
};
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE, Engine as _};
use hypervisor::{
    device::{
        device_manager::{do_handle_device, get_block_device_info, DeviceManager},
        DeviceConfig,
    },
    BlockConfigModern, BlockDeviceAio,
};
use kata_sys_util::mount::get_mount_path;
use kata_types::mount::DEFAULT_KATA_GUEST_SANDBOX_DIR;
use nix::sys::{stat, stat::SFlag};
use oci_spec::runtime as oci;
use tokio::sync::RwLock;

#[derive(Clone)]
pub(crate) struct BlockVolume {
    storage: Option<agent::Storage>,
    mount: oci::Mount,
    device_id: String,
}

/// BlockVolume for bind-mount block volume
impl BlockVolume {
    pub(crate) async fn new(
        d: &RwLock<DeviceManager>,
        m: &oci::Mount,
        read_only: bool,
        sid: &str,
    ) -> Result<Self> {
        let mnt_src = match m.source() {
            Some(path) => path,
            None => return Err(anyhow!("mount source path is empty")),
        };

        let blkdev_info = get_block_device_info(d).await;
        let fstat = stat::stat(mnt_src).context(format!("stat {}", mnt_src.display()))?;

        // Honor the host block device's own read-only flag in addition to the
        // mount-derived intent, so a device marked read-only on the host is
        // exposed read-only to the guest.
        let read_only = read_only
            || is_block_device_readonly(mnt_src).unwrap_or_else(|e| {
                warn!(
                    sl!(),
                    "could not query block device read-only flag for {}: {:?}",
                    mnt_src.display(),
                    e
                );
                false
            });

        let block_device_config = BlockConfigModern {
            path_on_host: mnt_src.to_string_lossy().to_string(),
            major: stat::major(fstat.st_rdev) as i64,
            minor: stat::minor(fstat.st_rdev) as i64,
            is_readonly: read_only,
            driver_option: blkdev_info.block_device_driver,
            blkdev_aio: BlockDeviceAio::new(&blkdev_info.block_device_aio),
            num_queues: blkdev_info.num_queues,
            queue_size: blkdev_info.queue_size,
            logical_sector_size: blkdev_info.block_device_logical_sector_size,
            physical_sector_size: blkdev_info.block_device_physical_sector_size,
            ..Default::default()
        };

        // create and insert block device into Kata VM
        let device_info = do_handle_device(
            d,
            &DeviceConfig::BlockCfgModern(block_device_config.clone()),
        )
        .await
        .context("do handle device failed.")?;

        let block_volume =
            handle_block_volume(device_info, m, read_only, sid, DEFAULT_VOLUME_FS_TYPE, None)
                .await
                .context("do handle block volume failed")?;

        Ok(Self {
            storage: Some(block_volume.0),
            mount: block_volume.1,
            device_id: block_volume.2,
        })
    }

    pub(crate) async fn new_annotated(
        d: &RwLock<DeviceManager>,
        m: &oci::Mount,
        device_number: (i64, i64),
        read_only: bool,
        sid: &str,
        fs_type: &str,
    ) -> Result<Self> {
        let blkdev_info = get_block_device_info(d).await;
        let block_device_config = BlockConfigModern {
            major: device_number.0,
            minor: device_number.1,
            is_readonly: read_only,
            driver_option: blkdev_info.block_device_driver,
            blkdev_aio: BlockDeviceAio::new(&blkdev_info.block_device_aio),
            num_queues: blkdev_info.num_queues,
            queue_size: blkdev_info.queue_size,
            logical_sector_size: blkdev_info.block_device_logical_sector_size,
            physical_sector_size: blkdev_info.block_device_physical_sector_size,
            ..Default::default()
        };

        let device_info = do_handle_device(d, &DeviceConfig::BlockCfgModern(block_device_config))
            .await
            .context("plug annotated block device")?;

        let (mut storage, mut mount, device_id) =
            handle_block_volume(device_info, m, read_only, sid, fs_type, None)
                .await
                .context("handle annotated block device")?;
        configure_annotated_storage(&mut storage, &mut mount);

        Ok(Self {
            storage: Some(storage),
            mount,
            device_id,
        })
    }
}

fn configure_annotated_storage(storage: &mut agent::Storage, mount: &mut oci::Mount) {
    let encoded_source = URL_SAFE.encode(storage.source.as_bytes());
    let mount_point = format!(
        "{}/storage/{}",
        DEFAULT_KATA_GUEST_SANDBOX_DIR, encoded_source
    );
    storage.mount_point = mount_point.clone();
    storage.shared = false;
    mount.set_source(Some(PathBuf::from(mount_point)));
}

#[async_trait]
impl Volume for BlockVolume {
    fn get_volume_mount(&self) -> Result<Vec<oci::Mount>> {
        Ok(vec![self.mount.clone()])
    }

    fn get_storage(&self) -> Result<Vec<agent::Storage>> {
        let s = if let Some(s) = self.storage.as_ref() {
            vec![s.clone()]
        } else {
            vec![]
        };

        Ok(s)
    }

    async fn cleanup(&self, device_manager: &RwLock<DeviceManager>) -> Result<()> {
        device_manager
            .write()
            .await
            .try_remove_device(&self.device_id)
            .await
    }

    fn get_device_id(&self) -> Result<Option<String>> {
        Ok(Some(self.device_id.clone()))
    }
}

pub(crate) fn is_block_volume(m: &oci::Mount) -> bool {
    let mnt_type: Option<String> = m.typ().clone();

    if mnt_type.clone().is_none() || mnt_type.unwrap().as_str() != KATA_MOUNT_BIND_TYPE {
        return false;
    }

    match stat::stat(get_mount_path(m.source()).as_str()) {
        Ok(fstat) => SFlag::from_bits_truncate(fstat.st_mode) == SFlag::S_IFBLK,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotated_storage_uses_sandbox_path() {
        let mut storage = agent::Storage {
            source: "00/00".to_string(),
            ..Default::default()
        };
        let mut mount = oci::Mount::default();

        configure_annotated_storage(&mut storage, &mut mount);

        assert!(storage.driver_options.is_empty());
        assert_eq!(
            storage.mount_point,
            "/run/kata-containers/sandbox/storage/MDAvMDA="
        );
        assert_eq!(
            mount.source(),
            &Some(PathBuf::from(
                "/run/kata-containers/sandbox/storage/MDAvMDA="
            ))
        );
        assert!(!storage.shared);
    }
}
