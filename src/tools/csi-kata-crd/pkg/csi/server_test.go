// Copyright (c) 2026 Kata Containers contributors
//
// SPDX-License-Identifier: Apache-2.0

package csi

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	"kata-containers/csi-kata-crd/pkg/contract"

	csipb "github.com/container-storage-interface/spec/lib/go/csi"
)

func TestNodePublishRegistersLogicalOCIDevice(t *testing.T) {
	root := t.TempDir()
	target := filepath.Join(t.TempDir(), "target")
	server := New("node", root)
	options, err := contract.EncodeMountOptions([]string{"noatime"})
	if err != nil {
		t.Fatal(err)
	}

	_, err = server.NodePublishVolume(context.Background(), &csipb.NodePublishVolumeRequest{
		VolumeId:   "volume",
		TargetPath: target,
		VolumeCapability: &csipb.VolumeCapability{
			AccessType: &csipb.VolumeCapability_Mount{
				Mount: &csipb.VolumeCapability_MountVolume{FsType: "ext4"},
			},
		},
		VolumeContext: map[string]string{
			contract.AttributeDevicePath:   "/dev/kata-crd-test",
			contract.AttributeMountOptions: options,
		},
	})
	if err != nil {
		t.Fatal(err)
	}

	path := filepath.Join(
		root,
		base64.URLEncoding.EncodeToString([]byte(target)),
		mountInfoFileName,
	)
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var info mountInfo
	if err := json.Unmarshal(data, &info); err != nil {
		t.Fatal(err)
	}
	if info.VolumeType != contract.DirectVolumeType {
		t.Fatalf("unexpected volume type %q", info.VolumeType)
	}
	if info.Device != "/dev/kata-crd-test" {
		t.Fatalf("unexpected device %q", info.Device)
	}
	if len(info.Options) != 2 || info.Options[0] != "noatime" || info.Options[1] != "rw" {
		t.Fatalf("unexpected options %#v", info.Options)
	}

	if _, err := server.NodeUnpublishVolume(
		context.Background(),
		&csipb.NodeUnpublishVolumeRequest{VolumeId: "volume", TargetPath: target},
	); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(path); !os.IsNotExist(err) {
		t.Fatalf("mount info still exists: %v", err)
	}
}
