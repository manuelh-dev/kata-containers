// Copyright (c) 2026 Kata Containers contributors
//
// SPDX-License-Identifier: Apache-2.0

package controller

import (
	"context"
	"testing"
	"time"

	"kata-containers/csi-kata-crd/pkg/contract"

	corev1 "k8s.io/api/core/v1"
	storagev1 "k8s.io/api/storage/v1"
	"k8s.io/apimachinery/pkg/api/resource"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"k8s.io/apimachinery/pkg/runtime"
	"k8s.io/apimachinery/pkg/types"
	dynamicfake "k8s.io/client-go/dynamic/fake"
	kubernetesfake "k8s.io/client-go/kubernetes/fake"
)

func TestReconcileCreatesKataPersistentVolume(t *testing.T) {
	group := contract.APIGroup
	storageClassName := "kata-crd-ext4"
	blockMode := corev1.PersistentVolumeBlock
	filesystemMode := corev1.PersistentVolumeFilesystem
	claim := &corev1.PersistentVolumeClaim{
		ObjectMeta: metav1.ObjectMeta{
			Name:      "created",
			Namespace: "default",
			UID:       types.UID("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"),
		},
		Spec: corev1.PersistentVolumeClaimSpec{
			AccessModes:      []corev1.PersistentVolumeAccessMode{corev1.ReadWriteOnce},
			VolumeMode:       &filesystemMode,
			StorageClassName: &storageClassName,
			DataSourceRef: &corev1.TypedObjectReference{
				APIGroup: &group,
				Kind:     contract.KataVolumeKind,
				Name:     "created-kv",
			},
			Resources: corev1.VolumeResourceRequirements{
				Requests: corev1.ResourceList{
					corev1.ResourceStorage: resource.MustParse("1Gi"),
				},
			},
		},
	}
	backingClaim := &corev1.PersistentVolumeClaim{
		ObjectMeta: metav1.ObjectMeta{
			Name:      "created-disk",
			Namespace: "default",
			UID:       types.UID("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"),
		},
		Spec: corev1.PersistentVolumeClaimSpec{VolumeMode: &blockMode},
	}
	storageClass := &storagev1.StorageClass{
		ObjectMeta:  metav1.ObjectMeta{Name: storageClassName},
		Provisioner: contract.DriverName,
		Parameters: map[string]string{
			contract.FSTypeParameter: "ext4",
		},
		MountOptions: []string{"noatime"},
	}
	kataVolume := &unstructured.Unstructured{Object: map[string]interface{}{
		"apiVersion": contract.APIGroup + "/" + contract.APIVersion,
		"kind":       contract.KataVolumeKind,
		"metadata": map[string]interface{}{
			"name":      "created-kv",
			"namespace": "default",
			"uid":       "12345678-1234-1234-1234-123456789abc",
		},
		"spec": map[string]interface{}{
			"backingClaim": "created-disk",
		},
	}}
	kataVolume.SetGroupVersionKind(contract.KataVolumeGVR.GroupVersion().WithKind(contract.KataVolumeKind))

	kube := kubernetesfake.NewSimpleClientset(claim, backingClaim, storageClass)
	dynamicClient := dynamicfake.NewSimpleDynamicClient(runtime.NewScheme(), kataVolume)
	controller := New(kube, dynamicClient, time.Second)

	if err := controller.reconcileClaim(context.Background(), claim); err != nil {
		t.Fatal(err)
	}

	persistentVolume, err := kube.CoreV1().
		PersistentVolumes().
		Get(context.Background(), "kata-crd-aaaaaaaaaaaaaaaa", metav1.GetOptions{})
	if err != nil {
		t.Fatal(err)
	}
	if persistentVolume.Spec.CSI == nil {
		t.Fatal("created PV has no CSI source")
	}
	if persistentVolume.Spec.CSI.Driver != contract.DriverName {
		t.Fatalf("unexpected CSI driver %q", persistentVolume.Spec.CSI.Driver)
	}
	if persistentVolume.Spec.CSI.VolumeAttributes[contract.AttributeBackingClaim] != "created-disk" {
		t.Fatalf("unexpected attributes %#v", persistentVolume.Spec.CSI.VolumeAttributes)
	}
	if persistentVolume.Spec.CSI.VolumeAttributes[contract.AttributeDevicePath] !=
		"/dev/kata-crd-1234567812341234" {
		t.Fatalf("unexpected attributes %#v", persistentVolume.Spec.CSI.VolumeAttributes)
	}
}
