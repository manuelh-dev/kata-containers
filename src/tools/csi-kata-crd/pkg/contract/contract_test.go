// Copyright (c) 2026 Kata Containers contributors
//
// SPDX-License-Identifier: Apache-2.0

package contract

import (
	"testing"

	"k8s.io/apimachinery/pkg/types"
)

func TestResourceNamesAreDeterministic(t *testing.T) {
	uid := types.UID("12345678-1234-1234-1234-123456789abc")

	volumeName, err := BackingVolumeName(uid)
	if err != nil {
		t.Fatal(err)
	}
	devicePath, err := DevicePath(uid)
	if err != nil {
		t.Fatal(err)
	}

	if volumeName != "kata-backing-1234567812341234" {
		t.Fatalf("unexpected volume name %q", volumeName)
	}
	if devicePath != "/dev/kata-crd-1234567812341234" {
		t.Fatalf("unexpected device path %q", devicePath)
	}
}

func TestMountOptionsRoundTrip(t *testing.T) {
	encoded, err := EncodeMountOptions([]string{"noatime", "nodev"})
	if err != nil {
		t.Fatal(err)
	}
	decoded, err := DecodeMountOptions(encoded)
	if err != nil {
		t.Fatal(err)
	}
	if len(decoded) != 2 || decoded[0] != "noatime" || decoded[1] != "nodev" {
		t.Fatalf("unexpected options %#v", decoded)
	}
}
