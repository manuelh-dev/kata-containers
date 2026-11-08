// Copyright (c) 2026 Kata Containers contributors
//
// SPDX-License-Identifier: Apache-2.0

package contract

import (
	"encoding/json"
	"fmt"
	"strings"

	"k8s.io/apimachinery/pkg/runtime/schema"
	"k8s.io/apimachinery/pkg/types"
)

const (
	APIGroup         = "storage.katacontainers.io"
	APIVersion       = "v1alpha1"
	KataVolumeKind   = "KataVolume"
	DriverName       = "crdvolume.csi.katacontainers.io"
	DefaultFSType    = "ext4"
	FSTypeParameter  = "csi.storage.k8s.io/fstype"
	DirectVolumeType = "oci-block"

	AttributeBackingClaim    = APIGroup + "/backing-claim"
	AttributeBackingClaimUID = APIGroup + "/backing-claim-uid"
	AttributeDevicePath      = APIGroup + "/device-path"
	AttributeMountOptions    = APIGroup + "/mount-options"
)

var KataVolumeGVR = schema.GroupVersionResource{
	Group:    APIGroup,
	Version:  APIVersion,
	Resource: "katavolumes",
}

func ResourceID(uid types.UID) (string, error) {
	id := strings.ReplaceAll(string(uid), "-", "")
	if id == "" {
		return "", fmt.Errorf("resource UID is empty")
	}
	if len(id) > 16 {
		id = id[:16]
	}
	return id, nil
}

func BackingVolumeName(uid types.UID) (string, error) {
	id, err := ResourceID(uid)
	if err != nil {
		return "", err
	}
	return "kata-backing-" + id, nil
}

func DevicePath(uid types.UID) (string, error) {
	id, err := ResourceID(uid)
	if err != nil {
		return "", err
	}
	return "/dev/kata-crd-" + id, nil
}

func PersistentVolumeName(uid types.UID) (string, error) {
	id, err := ResourceID(uid)
	if err != nil {
		return "", err
	}
	return "kata-crd-" + id, nil
}

func EncodeMountOptions(options []string) (string, error) {
	value, err := json.Marshal(options)
	if err != nil {
		return "", fmt.Errorf("encode mount options: %w", err)
	}
	return string(value), nil
}

func DecodeMountOptions(value string) ([]string, error) {
	if value == "" {
		return nil, nil
	}
	var options []string
	if err := json.Unmarshal([]byte(value), &options); err != nil {
		return nil, fmt.Errorf("decode mount options: %w", err)
	}
	return options, nil
}
