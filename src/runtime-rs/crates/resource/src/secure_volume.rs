// Copyright (c) 2026 NVIDIA Corporation
//
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashMap, path::Path};

use anyhow::{anyhow, bail, Context, Result};
use kata_types::annotations::KATA_ANNO_CONFIDENTIAL_VOLUME;
use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfidentialVolume {
    pub annotation_key: String,
    pub manifest_uri: String,
    pub device_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct AnnotationValue {
    manifest_uri: String,
    device_path: String,
}

pub fn parse_annotations(
    annotations: &HashMap<String, String>,
    feature_enabled: bool,
) -> Result<HashMap<String, ConfidentialVolume>> {
    let Some(raw_value) = annotations.get(KATA_ANNO_CONFIDENTIAL_VOLUME) else {
        return Ok(HashMap::new());
    };
    if !feature_enabled {
        bail!("confidential block-device annotation used while feature is disabled");
    }

    let entries: HashMap<String, AnnotationValue> = serde_json::from_str(raw_value)
        .with_context(|| {
            format!("parse confidential volume annotation {KATA_ANNO_CONFIDENTIAL_VOLUME}")
        })?;
    if entries.is_empty() {
        bail!("confidential volume annotation must select at least one volume");
    }

    let mut result = HashMap::with_capacity(entries.len());
    for (volume_name, value) in entries {
        validate_volume_name(&volume_name)?;
        validate_device_path(&value.device_path)?;
        validate_manifest_uri(&value.manifest_uri)?;

        let volume = ConfidentialVolume {
            annotation_key: KATA_ANNO_CONFIDENTIAL_VOLUME.to_string(),
            manifest_uri: value.manifest_uri,
            device_path: value.device_path.clone(),
        };
        if result.insert(value.device_path.clone(), volume).is_some() {
            bail!(
                "confidential volumes in annotation {} both select {}",
                KATA_ANNO_CONFIDENTIAL_VOLUME,
                value.device_path
            );
        }
    }
    Ok(result)
}

fn validate_volume_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 63 {
        bail!("confidential volume annotation suffix must contain 1 to 63 characters");
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.'))
    {
        bail!("confidential volume annotation suffix contains invalid characters");
    }
    if !name
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        || !name
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
    {
        bail!("confidential volume annotation suffix must start and end with an alphanumeric character");
    }
    Ok(())
}

fn validate_device_path(device_path: &str) -> Result<()> {
    let path = Path::new(device_path);
    if !path.is_absolute() || !path.starts_with("/dev") || path == Path::new("/dev") {
        bail!("confidential volume devicePath must be an absolute path below /dev");
    }
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    }) {
        bail!("confidential volume devicePath must be canonical");
    }
    Ok(())
}

fn validate_manifest_uri(manifest_uri: &str) -> Result<()> {
    let parsed = url::Url::parse(manifest_uri)
        .map_err(|e| anyhow!("invalid confidential volume manifest URI: {e}"))?;
    if parsed.scheme() != "kbs" || parsed.path().trim_matches('/').is_empty() {
        bail!("confidential volume manifestUri must be a kbs resource URI");
    }
    if parsed.username() != "" || parsed.password().is_some() || parsed.fragment().is_some() {
        bail!("confidential volume manifestUri contains unsupported URI components");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_enabled_annotation() {
        let annotations = HashMap::from([(
            KATA_ANNO_CONFIDENTIAL_VOLUME.to_string(),
            r#"{"block-disk":{"manifestUri":"kbs:///kata-ci/storage-manifest/model-v1","devicePath":"/dev/hostdisk"}}"#
                .to_string(),
        )]);
        let parsed = parse_annotations(&annotations, true).unwrap();
        assert_eq!(
            parsed["/dev/hostdisk"].manifest_uri,
            "kbs:///kata-ci/storage-manifest/model-v1"
        );
    }

    #[test]
    fn annotation_fails_when_feature_is_disabled() {
        let annotations = HashMap::from([(
            KATA_ANNO_CONFIDENTIAL_VOLUME.to_string(),
            r#"{"block-disk":{"manifestUri":"kbs:///a/b/c","devicePath":"/dev/hostdisk"}}"#
                .to_string(),
        )]);
        assert!(parse_annotations(&annotations, false).is_err());
    }

    #[test]
    fn rejects_two_volumes_for_one_device() {
        let annotations = HashMap::from([(
            KATA_ANNO_CONFIDENTIAL_VOLUME.to_string(),
            r#"{
                "first":{"manifestUri":"kbs:///a/b/c","devicePath":"/dev/hostdisk"},
                "second":{"manifestUri":"kbs:///a/b/c","devicePath":"/dev/hostdisk"}
            }"#
            .to_string(),
        )]);
        assert!(parse_annotations(&annotations, true).is_err());
    }

    #[test]
    fn rejects_empty_volume_map() {
        let annotations = HashMap::from([(
            KATA_ANNO_CONFIDENTIAL_VOLUME.to_string(),
            "{}".to_string(),
        )]);
        assert!(parse_annotations(&annotations, true).is_err());
    }
}
