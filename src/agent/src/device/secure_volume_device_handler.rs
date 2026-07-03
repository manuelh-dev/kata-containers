// Copyright (c) 2026 NVIDIA Corporation
//
// SPDX-License-Identifier: Apache-2.0

use crate::confidential_data_hub;
use crate::device::{DeviceContext, DeviceHandler, DeviceInfo, SpecUpdate, DEVICE_HANDLERS};
use anyhow::{anyhow, bail, Context, Result};
use kata_types::device::DRIVER_SECURE_VOLUME_TYPE;
use protocols::agent::Device;

#[derive(Debug)]
pub struct SecureVolumeDeviceHandler {}

#[async_trait::async_trait]
impl DeviceHandler for SecureVolumeDeviceHandler {
    fn driver_types(&self) -> &[&str] {
        &[DRIVER_SECURE_VOLUME_TYPE]
    }

    async fn device_handler(&self, device: &Device, ctx: &mut DeviceContext) -> Result<SpecUpdate> {
        let secure = device
            .secure_volume
            .as_ref()
            .context("secure-volume device is missing its activation configuration")?;
        if secure.source_driver.is_empty() || secure.source_driver == DRIVER_SECURE_VOLUME_TYPE {
            bail!("secure-volume device has an invalid source driver");
        }
        if secure.annotation_key.is_empty() || secure.manifest_uri.is_empty() {
            bail!("secure-volume device has incomplete activation configuration");
        }

        let source_handler = DEVICE_HANDLERS
            .handler(&secure.source_driver)
            .ok_or_else(|| {
                anyhow!(
                    "unsupported secure-volume source driver {}",
                    secure.source_driver
                )
            })?;
        let mut source_device = device.clone();
        source_device.type_ = secure.source_driver.clone();
        source_device.secure_volume = Default::default();
        let source_update = source_handler
            .device_handler(&source_device, ctx)
            .await
            .with_context(|| {
                format!(
                    "resolve secure-volume source device using {}",
                    secure.source_driver
                )
            })?;
        let source_info = source_update
            .dev
            .context("secure-volume source handler returned no block device")?
            .info;
        if source_info.cgroup_type != "b" {
            bail!("secure-volume source is not a block device");
        }

        let device_id = format!("{}:{}", source_info.guest_major, source_info.guest_minor);
        let (activation_id, mapper_path) =
            confidential_data_hub::activate_volume(&device_id, &secure.manifest_uri)
                .await
                .with_context(|| format!("activate confidential block device {device_id}"))?;
        if activation_id.is_empty() || mapper_path.is_empty() {
            if !activation_id.is_empty() {
                let _ = confidential_data_hub::deactivate_volume(&activation_id).await;
            }
            bail!("CDH returned an incomplete secure-volume activation");
        }

        let mapper_info = match DeviceInfo::new(&mapper_path, true)
            .with_context(|| format!("inspect activated device {mapper_path}"))
        {
            Ok(info) if info.cgroup_type == "b" => info,
            Ok(_) => {
                let _ = confidential_data_hub::deactivate_volume(&activation_id).await;
                bail!("CDH activation path is not a block device");
            }
            Err(error) => {
                let _ = confidential_data_hub::deactivate_volume(&activation_id).await;
                return Err(error);
            }
        };

        ctx.sandbox
            .lock()
            .await
            .container_secure_volume_activations
            .entry(ctx.cid.to_string())
            .or_default()
            .push(activation_id);
        Ok(mapper_info.into())
    }
}
