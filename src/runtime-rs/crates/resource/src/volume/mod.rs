// Copyright (c) 2019-2022 Alibaba Cloud
// Copyright (c) 2019-2022 Ant Group
//
// SPDX-License-Identifier: Apache-2.0
//

pub(crate) mod block_emptydir_volume;
mod block_volume;
mod default_volume;
mod ephemeral_volume;
pub mod hugepage;
mod local_volume;
mod share_fs_volume;
mod shm_volume;
pub mod utils;

pub mod direct_volume;
use crate::volume::{direct_volume::is_direct_volume, share_fs_volume::VolumeManager};
pub mod direct_volumes;

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
    vec::Vec,
};

use self::hugepage::{get_huge_page_limits_map, get_huge_page_option};
use crate::{share_fs::ShareFs, volume::block_volume::is_block_volume};
use agent::Agent;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use hypervisor::device::device_manager::DeviceManager;
use kata_sys_util::{k8s::is_disk_empty_dir, mount::get_mount_options};
use kata_types::{
    annotations::block_device_mount::{
        parse_block_device_mounts, BlockDeviceMount, BLOCK_DEVICE_MOUNTS_ANNOTATION,
    },
    k8s,
};
use oci_spec::runtime::{self as oci, LinuxDeviceType};
use tokio::sync::RwLock;

const BIND: &str = "bind";

pub struct VolumeContext<'a> {
    pub share_fs: &'a Option<Arc<dyn ShareFs>>,
    pub d: &'a RwLock<DeviceManager>,
    pub sid: &'a str,
    pub agent: Arc<dyn Agent>,
    pub emptydir_mode: &'a str,
    pub fs_sharing_supported: bool,
    pub block_device_discard_supported: bool,
}

#[async_trait]
pub trait Volume: Send + Sync {
    fn get_volume_mount(&self) -> Result<Vec<oci::Mount>>;
    fn get_storage(&self) -> Result<Vec<agent::Storage>>;
    fn get_device_id(&self) -> Result<Option<String>>;
    async fn cleanup(&self, device_manager: &RwLock<DeviceManager>) -> Result<()>;
}

#[derive(Default)]
pub struct VolumeResourceInner {
    volumes: Vec<Arc<dyn Volume>>,
    ephemeral_disks: Vec<block_emptydir_volume::EphemeralDiskInfo>,
}

#[derive(Default)]
pub struct VolumeResource {
    inner: Arc<RwLock<VolumeResourceInner>>,
    // The core purpose of introducing `volume_manager` to `VolumeResource` is to centralize the management of shared file system volumes.
    // By creating a single VolumeManager instance within VolumeResource, all shared file volumes are managed by one central entity.
    // This single volume_manager can accurately track the references of all ShareFsVolume instances to the shared volumes,
    // ensuring correct reference counting, proper volume lifecycle management, and preventing issues like volumes being overwritten.
    volume_manager: Arc<VolumeManager>,
}

impl VolumeResource {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(VolumeResourceInner::default())),
            volume_manager: Arc::new(VolumeManager::new()),
        }
    }

    pub async fn handler_volumes(
        &self,
        ctx: &VolumeContext<'_>,
        cid: &str,
        spec: &mut oci::Spec,
    ) -> Result<Vec<Arc<dyn Volume>>> {
        let mut volumes = vec![];
        if let Err(e) = self.add_volumes(ctx, cid, spec, &mut volumes).await {
            // On error the caller never receives these volumes and cannot clean them up.
            for v in volumes.iter().rev() {
                if let Err(err) = v.cleanup(ctx.d).await {
                    warn!(sl!(), "failed to clean up volume: {:?}", err);
                }
            }
            return Err(e);
        }
        Ok(volumes)
    }

    async fn add_volumes(
        &self,
        ctx: &VolumeContext<'_>,
        cid: &str,
        spec: &mut oci::Spec,
        volumes: &mut Vec<Arc<dyn Volume>>,
    ) -> Result<()> {
        let annotated_plans = annotated_block_device_plans(spec)?;
        let share_fs = ctx.share_fs;
        let d = ctx.d;
        let sid = ctx.sid;
        let emptydir_mode = ctx.emptydir_mode;
        let fs_sharing_supported = ctx.fs_sharing_supported;
        let oci_mounts = &spec.mounts().clone().unwrap_or_default();
        info!(sl!(), " oci mount is : {:?}", oci_mounts.clone());
        // handle mounts
        for m in oci_mounts {
            let read_only = get_mount_options(m.options()).iter().any(|opt| opt == "ro");
            let volume: Arc<dyn Volume> = if shm_volume::is_shm_volume(m) {
                Arc::new(
                    shm_volume::ShmVolume::new(m)
                        .with_context(|| format!("new shm volume {m:?}"))?,
                )
            } else if ephemeral_volume::is_ephemeral_volume(m) {
                Arc::new(
                    ephemeral_volume::EphemeralVolume::new(m)
                        .with_context(|| format!("new ephemeral volume {m:?}"))?,
                )
            } else if block_emptydir_volume::is_block_emptydir_volume(m, emptydir_mode) {
                let vol = block_emptydir_volume::BlockEmptyDirVolume::new(
                    d,
                    m,
                    sid,
                    emptydir_mode,
                    ctx.block_device_discard_supported,
                )
                .await
                .with_context(|| format!("new block emptydir volume {m:?}"))?;
                let vol_arc: Arc<dyn Volume> = Arc::new(vol.clone());
                let mut inner = self.inner.write().await;
                inner.ephemeral_disks.push(vol.disk_info);
                drop(inner);
                vol_arc
            } else if need_local_volume(m, fs_sharing_supported, emptydir_mode) {
                // This branch comes after is_block_emptydir_volume() so
                // block-encrypted and block-plain emptyDirs are handled as
                // block devices before falling back to guest-local storage.
                warn!(
                    sl!(),
                    "handling emptyDir as guest-local volume because fs sharing is unsupported; Kubelet cannot enforce sizeLimit-based eviction",
                );
                Arc::new(
                    local_volume::LocalStorage::new(m, sid, cid)
                        .with_context(|| format!("new local volume {m:?}"))?,
                )
            } else if is_block_volume(m) {
                // handle block volume
                Arc::new(
                    block_volume::BlockVolume::new(d, m, read_only, sid)
                        .await
                        .with_context(|| format!("new block volume {m:?}"))?,
                )
            } else if is_direct_volume(m)? {
                // handle direct volumes
                match direct_volume::handle_direct_volume(d, m, read_only, sid)
                    .await
                    .context("handle direct volume")?
                {
                    Some(directvol) => directvol,
                    None => continue,
                }
            } else if let Some(options) =
                get_huge_page_option(m).context("failed to check huge page")?
            {
                // get hugepage limits from oci
                let hugepage_limits =
                    get_huge_page_limits_map(spec).context("get huge page option")?;
                // handle container hugepage
                Arc::new(
                    hugepage::Hugepage::new(m, hugepage_limits, options)
                        .with_context(|| format!("handle hugepages {m:?}"))?,
                )
            } else if share_fs_volume::is_share_fs_volume(m) {
                Arc::new(
                    share_fs_volume::ShareFsVolume::new(
                        share_fs,
                        m,
                        cid,
                        read_only,
                        ctx.agent.clone(),
                        self.volume_manager.clone(),
                    )
                    .await
                    .with_context(|| format!("new share fs volume {m:?}"))?,
                )
            } else if is_skip_volume(m) {
                info!(sl!(), "skip volume {:?}", m);
                continue;
            } else {
                Arc::new(
                    default_volume::DefaultVolume::new(m)
                        .with_context(|| format!("new default volume {m:?}"))?,
                )
            };

            volumes.push(volume.clone());
            let mut inner = self.inner.write().await;
            inner.volumes.push(volume);
        }

        let mut transformed_device_paths = HashSet::new();
        for plan in annotated_plans {
            let mut mount = oci::Mount::default();
            mount.set_destination(PathBuf::from(&plan.config.mount_path));
            mount.set_typ(Some(BIND.to_string()));
            mount.set_options(Some(plan.config.mount_options.clone()));

            let volume: Arc<dyn Volume> = Arc::new(
                block_volume::BlockVolume::new_annotated(
                    d,
                    &mount,
                    (plan.major, plan.minor),
                    plan.read_only,
                    sid,
                    &plan.config.fs_type,
                )
                .await
                .with_context(|| {
                    format!(
                        "mount annotated block device {} for volume {}",
                        plan.config.device_path, plan.config.volume
                    )
                })?,
            );
            transformed_device_paths.insert(plan.config.device_path);
            volumes.push(volume.clone());
            self.inner.write().await.volumes.push(volume);
        }

        if !transformed_device_paths.is_empty() {
            let linux = spec
                .linux_mut()
                .as_mut()
                .context("OCI spec missing linux field")?;
            let mut devices = linux.devices().clone().unwrap_or_default();
            devices.retain(|device| {
                !transformed_device_paths.contains(&device.path().display().to_string())
            });
            linux.set_devices(Some(devices));
        }

        Ok(())
    }

    pub async fn cleanup_ephemeral_disks(&self) -> Result<()> {
        let inner = self.inner.read().await;
        let mut failures = 0;
        for disk in &inner.ephemeral_disks {
            if let Err(e) = std::fs::remove_file(&disk.disk_path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    failures += 1;
                    warn!(
                        sl!(),
                        "failed to remove ephemeral disk {:?}: {}", disk.disk_path, e
                    );
                }
            }
            if let Err(e) = kata_types::mount::remove_volume_path(&disk.source_path) {
                failures += 1;
                warn!(
                    sl!(),
                    "failed to remove direct-volume path for {}: {}", disk.source_path, e
                );
            }
        }
        if failures == 0 {
            Ok(())
        } else {
            Err(anyhow!(
                "{failures} ephemeral disk cleanup operation(s) failed"
            ))
        }
    }

    pub async fn dump(&self) {
        let inner = self.inner.read().await;
        for v in &inner.volumes {
            info!(
                sl!(),
                "volume mount {:?}: count {}",
                v.get_volume_mount(),
                Arc::strong_count(v)
            );
        }
    }
}

#[derive(Debug)]
struct AnnotatedBlockDevicePlan {
    config: BlockDeviceMount,
    major: i64,
    minor: i64,
    read_only: bool,
}

fn annotated_block_device_plans(spec: &oci::Spec) -> Result<Vec<AnnotatedBlockDevicePlan>> {
    let annotation = match spec
        .annotations()
        .as_ref()
        .and_then(|annotations| annotations.get(BLOCK_DEVICE_MOUNTS_ANNOTATION))
    {
        Some(annotation) => annotation,
        None => return Ok(Vec::new()),
    };
    let config = parse_block_device_mounts(annotation).map_err(|e| anyhow!(e))?;
    let container_name = k8s::container_name(spec);
    let mounts = spec.mounts().clone().unwrap_or_default();
    let linux = spec
        .linux()
        .as_ref()
        .context("OCI spec missing linux field")?;
    let devices = linux.devices().clone().unwrap_or_default();

    let mut plans = Vec::new();
    for mount in config
        .mounts
        .into_iter()
        .filter(|mount| mount.container == container_name)
    {
        if mounts
            .iter()
            .any(|existing| existing.destination().as_path() == Path::new(&mount.mount_path))
        {
            return Err(anyhow!(
                "block-device-mount destination {} conflicts with an OCI mount",
                mount.mount_path
            ));
        }

        let matching: Vec<_> = devices
            .iter()
            .filter(|device| {
                device.typ() == LinuxDeviceType::B
                    && device.path().display().to_string() == mount.device_path
            })
            .collect();
        if matching.len() != 1 {
            return Err(anyhow!(
                "block-device-mount volume {} expected exactly one block device at {}, found {}",
                mount.volume,
                mount.device_path,
                matching.len()
            ));
        }
        let device = matching[0];
        let read_only = crate::manager_inner::device_cgroup_access_is_readonly(
            linux,
            LinuxDeviceType::B,
            device.major(),
            device.minor(),
        ) || crate::manager_inner::block_device_node_is_readonly(
            device.major(),
            device.minor(),
        );
        if read_only && !mount.mount_options.iter().any(|option| option == "ro") {
            return Err(anyhow!(
                "block-device-mount volume {} is read-only; mountOptions must include ro",
                mount.volume
            ));
        }

        plans.push(AnnotatedBlockDevicePlan {
            config: mount,
            major: device.major(),
            minor: device.minor(),
            read_only,
        });
    }

    Ok(plans)
}

/// Indicates whether a mount needs to be a local volume, i.e. created
/// inside the guest instead of being shared from the host.
///
/// This returns true when the hypervisor doesn't support fs sharing
/// (e.g. peer pods) and the mount is a non-block-based disk-backed
/// emptyDir.
///
/// Limitation: Local volumes cannot be managed by Kubelet and hence may
/// starve the host storage.
fn need_local_volume(m: &oci::Mount, fs_sharing_supported: bool, emptydir_mode: &str) -> bool {
    !fs_sharing_supported
        && !block_emptydir_volume::is_block_emptydir_mode(emptydir_mode)
        && m.source()
            .as_ref()
            .is_some_and(|src| is_disk_empty_dir(&src.display().to_string()))
}

fn is_skip_volume(_m: &oci::Mount) -> bool {
    // TODO: support volume check
    false
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;
    use std::collections::HashMap;

    use oci_spec::runtime::{LinuxBuilder, LinuxDeviceBuilder, SpecBuilder};

    #[tokio::test]
    async fn failed_ephemeral_disk_removal_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let disk_path = dir.path().join("disk.img");
        std::fs::create_dir(&disk_path).unwrap();

        let resource = VolumeResource::new();
        resource.inner.write().await.ephemeral_disks.push(
            block_emptydir_volume::EphemeralDiskInfo {
                disk_path: disk_path.clone(),
                source_path: format!(
                    "ephemeral-test-{}",
                    dir.path().file_name().unwrap().to_string_lossy()
                ),
            },
        );

        assert!(resource.cleanup_ephemeral_disks().await.is_err());
    }

    fn annotated_spec(device_path: &str) -> oci::Spec {
        let annotation = format!(
            r#"{{"mounts":[{{"container":"app","volume":"data","devicePath":"{device_path}","mountPath":"/data","fsType":"ext4","mountOptions":["nosuid"]}}]}}"#
        );
        let device = LinuxDeviceBuilder::default()
            .path(PathBuf::from("/dev/kata-data"))
            .typ(LinuxDeviceType::B)
            .major(8_i64)
            .minor(0_i64)
            .build()
            .unwrap();
        SpecBuilder::default()
            .annotations(HashMap::from([
                (
                    kata_types::annotations::cri_containerd::CONTAINER_NAME_LABEL_KEY.to_string(),
                    "app".to_string(),
                ),
                (BLOCK_DEVICE_MOUNTS_ANNOTATION.to_string(), annotation),
            ]))
            .linux(
                LinuxBuilder::default()
                    .devices(vec![device])
                    .build()
                    .unwrap(),
            )
            .build()
            .unwrap()
    }

    #[test]
    fn annotation_matches_linux_block_device() {
        let plans = annotated_block_device_plans(&annotated_spec("/dev/kata-data")).unwrap();

        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].major, 8);
        assert_eq!(plans[0].config.mount_path, "/data");
    }

    #[test]
    fn annotation_rejects_missing_linux_block_device() {
        let err = annotated_block_device_plans(&annotated_spec("/dev/missing")).unwrap_err();

        assert!(err.to_string().contains("found 0"));
    }
}
