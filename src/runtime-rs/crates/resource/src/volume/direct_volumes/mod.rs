// Copyright (c) 2023 Ant Group
//
// SPDX-License-Identifier: Apache-2.0
//

use anyhow::{anyhow, Context, Result};

use kata_types::mount::{
    get_volume_mount_info, join_path, kata_direct_volume_root_path, DirectVolumeMountInfo,
    KATA_BLOCK_VOLUME_CREATE_FS, KATA_BLOCK_VOLUME_STORAGE_LIFECYCLE,
    KATA_BLOCK_VOLUME_STORAGE_LIFECYCLE_PERSISTENT,
};

pub mod rawblock_volume;
pub mod spdk_volume;
pub mod vfio_volume;

pub const KATA_DIRECT_VOLUME_TYPE: &str = "directvol";
pub const KATA_VFIO_VOLUME_TYPE: &str = "vfiovol";
pub const KATA_SPDK_VOLUME_TYPE: &str = "spdkvol";
pub const KATA_SPOOL_VOLUME_TYPE: &str = "spoolvol";

const METADATA_CREATE_FILESYSTEM: &str = "createFilesystem";
const METADATA_STORAGE_LIFECYCLE: &str = "storageLifecycle";

pub(crate) fn configure_storage_driver_options(
    storage: &mut agent::Storage,
    mount_info: &DirectVolumeMountInfo,
) -> Result<()> {
    if let Some(create_filesystem) = mount_info.metadata.get(METADATA_CREATE_FILESYSTEM) {
        if create_filesystem
            .parse::<bool>()
            .context("parse createFilesystem metadata")?
        {
            storage
                .driver_options
                .push(KATA_BLOCK_VOLUME_CREATE_FS.to_string());
        }
    }

    if let Some(storage_lifecycle) = mount_info.metadata.get(METADATA_STORAGE_LIFECYCLE) {
        if storage_lifecycle != KATA_BLOCK_VOLUME_STORAGE_LIFECYCLE_PERSISTENT {
            return Err(anyhow!(
                "unsupported storage lifecycle {storage_lifecycle:?}"
            ));
        }
        storage.driver_options.push(format!(
            "{KATA_BLOCK_VOLUME_STORAGE_LIFECYCLE}={storage_lifecycle}"
        ));
    }

    Ok(())
}

// volume mount info load infomation from mountinfo.json
pub fn volume_mount_info(volume_path: &str) -> Result<DirectVolumeMountInfo> {
    get_volume_mount_info(volume_path)
}

// get direct volume path whose volume_path encoded with base64
pub fn get_direct_volume_path(volume_path: &str) -> Result<String> {
    let volume_full_path = join_path(kata_direct_volume_root_path().as_str(), volume_path)
        .context("failed to join path.")?;

    if volume_full_path.exists() {
        Ok(volume_full_path.display().to_string())
    } else {
        Err(anyhow!(format!(
            "direct volume path  {:?} Not Found",
            volume_full_path
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistent_filesystem_creation_options_are_forwarded() {
        let mut mount_info = DirectVolumeMountInfo::default();
        mount_info
            .metadata
            .insert(METADATA_CREATE_FILESYSTEM.to_string(), "true".to_string());
        mount_info.metadata.insert(
            METADATA_STORAGE_LIFECYCLE.to_string(),
            KATA_BLOCK_VOLUME_STORAGE_LIFECYCLE_PERSISTENT.to_string(),
        );
        let mut storage = agent::Storage::default();

        configure_storage_driver_options(&mut storage, &mount_info).unwrap();

        assert_eq!(
            storage.driver_options,
            vec![
                KATA_BLOCK_VOLUME_CREATE_FS.to_string(),
                format!(
                    "{KATA_BLOCK_VOLUME_STORAGE_LIFECYCLE}={KATA_BLOCK_VOLUME_STORAGE_LIFECYCLE_PERSISTENT}"
                ),
            ]
        );
    }

}
