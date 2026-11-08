// Copyright (c) 2026 Kata Containers contributors
//
// SPDX-License-Identifier: Apache-2.0

package controller

import (
	"context"
	"fmt"
	"log"
	"time"

	"kata-containers/csi-kata-crd/pkg/contract"

	corev1 "k8s.io/api/core/v1"
	apierrors "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"k8s.io/apimachinery/pkg/types"
	"k8s.io/client-go/dynamic"
	"k8s.io/client-go/kubernetes"
)

type Controller struct {
	kube     kubernetes.Interface
	dynamic  dynamic.Interface
	interval time.Duration
}

func New(kube kubernetes.Interface, dynamicClient dynamic.Interface, interval time.Duration) *Controller {
	return &Controller{
		kube:     kube,
		dynamic:  dynamicClient,
		interval: interval,
	}
}

func (controller *Controller) Run(ctx context.Context) error {
	ticker := time.NewTicker(controller.interval)
	defer ticker.Stop()

	for {
		if err := controller.reconcile(ctx); err != nil {
			log.Printf("reconcile Kata PVCs: %v", err)
		}

		select {
		case <-ctx.Done():
			return nil
		case <-ticker.C:
		}
	}
}

func (controller *Controller) reconcile(ctx context.Context) error {
	claims, err := controller.kube.CoreV1().PersistentVolumeClaims("").List(ctx, metav1.ListOptions{})
	if err != nil {
		return fmt.Errorf("list PVCs: %w", err)
	}

	for index := range claims.Items {
		claim := &claims.Items[index]
		if !isKataVolumeClaim(claim) {
			continue
		}
		if err := controller.reconcileClaim(ctx, claim); err != nil {
			log.Printf("reconcile PVC %s/%s: %v", claim.Namespace, claim.Name, err)
		}
	}
	return nil
}

func isKataVolumeClaim(claim *corev1.PersistentVolumeClaim) bool {
	source := claim.Spec.DataSourceRef
	return source != nil &&
		source.APIGroup != nil &&
		*source.APIGroup == contract.APIGroup &&
		source.Kind == contract.KataVolumeKind
}

func (controller *Controller) reconcileClaim(
	ctx context.Context,
	claim *corev1.PersistentVolumeClaim,
) error {
	source := claim.Spec.DataSourceRef
	kataVolume, err := controller.dynamic.Resource(contract.KataVolumeGVR).
		Namespace(claim.Namespace).
		Get(ctx, source.Name, metav1.GetOptions{})
	if err != nil {
		return fmt.Errorf("get KataVolume %q: %w", source.Name, err)
	}

	backingClaimName, found, err := unstructured.NestedString(
		kataVolume.Object,
		"spec",
		"backingClaim",
	)
	if err != nil || !found || backingClaimName == "" {
		return fmt.Errorf("KataVolume %q has no spec.backingClaim", source.Name)
	}
	backingClaim, err := controller.kube.CoreV1().
		PersistentVolumeClaims(claim.Namespace).
		Get(ctx, backingClaimName, metav1.GetOptions{})
	if err != nil {
		return fmt.Errorf("get backing PVC %q: %w", backingClaimName, err)
	}
	if backingClaim.Spec.VolumeMode == nil ||
		*backingClaim.Spec.VolumeMode != corev1.PersistentVolumeBlock {
		return fmt.Errorf("backing PVC %q must have volumeMode Block", backingClaimName)
	}
	if claim.Spec.VolumeMode != nil &&
		*claim.Spec.VolumeMode != corev1.PersistentVolumeFilesystem {
		return fmt.Errorf("Kata-facing PVC must have volumeMode Filesystem")
	}
	if claim.Spec.StorageClassName == nil || *claim.Spec.StorageClassName == "" {
		return fmt.Errorf("Kata-facing PVC has no storageClassName")
	}

	storageClass, err := controller.kube.StorageV1().
		StorageClasses().
		Get(ctx, *claim.Spec.StorageClassName, metav1.GetOptions{})
	if err != nil {
		return fmt.Errorf("get StorageClass %q: %w", *claim.Spec.StorageClassName, err)
	}
	if storageClass.Provisioner != contract.DriverName {
		return fmt.Errorf(
			"StorageClass %q uses provisioner %q, expected %q",
			storageClass.Name,
			storageClass.Provisioner,
			contract.DriverName,
		)
	}

	fsType := storageClass.Parameters[contract.FSTypeParameter]
	if fsType == "" {
		fsType = contract.DefaultFSType
	}
	mountOptions, err := contract.EncodeMountOptions(storageClass.MountOptions)
	if err != nil {
		return err
	}
	devicePath, err := contract.DevicePath(kataVolume.GetUID())
	if err != nil {
		return err
	}
	persistentVolumeName, err := contract.PersistentVolumeName(claim.UID)
	if err != nil {
		return err
	}

	volumeMode := corev1.PersistentVolumeFilesystem
	persistentVolume := &corev1.PersistentVolume{
		ObjectMeta: metav1.ObjectMeta{
			Name: persistentVolumeName,
			Labels: map[string]string{
				contract.APIGroup + "/claim-uid": string(claim.UID),
			},
		},
		Spec: corev1.PersistentVolumeSpec{
			Capacity:                      claim.Spec.Resources.Requests.DeepCopy(),
			AccessModes:                   append([]corev1.PersistentVolumeAccessMode(nil), claim.Spec.AccessModes...),
			PersistentVolumeReclaimPolicy: corev1.PersistentVolumeReclaimRetain,
			StorageClassName:              storageClass.Name,
			MountOptions:                  append([]string(nil), storageClass.MountOptions...),
			VolumeMode:                    &volumeMode,
			ClaimRef: &corev1.ObjectReference{
				APIVersion: "v1",
				Kind:       "PersistentVolumeClaim",
				Namespace:  claim.Namespace,
				Name:       claim.Name,
				UID:        claim.UID,
			},
			PersistentVolumeSource: corev1.PersistentVolumeSource{
				CSI: &corev1.CSIPersistentVolumeSource{
					Driver:       contract.DriverName,
					VolumeHandle: fmt.Sprintf("%s/%s", claim.Namespace, kataVolume.GetUID()),
					FSType:       fsType,
					VolumeAttributes: map[string]string{
						contract.AttributeBackingClaim:    backingClaim.Name,
						contract.AttributeBackingClaimUID: string(backingClaim.UID),
						contract.AttributeDevicePath:      devicePath,
						contract.AttributeMountOptions:    mountOptions,
					},
				},
			},
		},
	}

	if _, err := controller.kube.CoreV1().
		PersistentVolumes().
		Create(ctx, persistentVolume, metav1.CreateOptions{}); err != nil &&
		!apierrors.IsAlreadyExists(err) {
		return fmt.Errorf("create PV %q: %w", persistentVolumeName, err)
	}

	return controller.updateStatus(ctx, kataVolume, backingClaim.UID, devicePath)
}

func (controller *Controller) updateStatus(
	ctx context.Context,
	kataVolume *unstructured.Unstructured,
	backingClaimUID types.UID,
	devicePath string,
) error {
	currentBackingClaimUID, _, _ := unstructured.NestedString(
		kataVolume.Object,
		"status",
		"backingClaimUID",
	)
	currentDevicePath, _, _ := unstructured.NestedString(
		kataVolume.Object,
		"status",
		"devicePath",
	)
	if currentBackingClaimUID == string(backingClaimUID) && currentDevicePath == devicePath {
		return nil
	}

	updated := kataVolume.DeepCopy()
	if err := unstructured.SetNestedField(
		updated.Object,
		string(backingClaimUID),
		"status",
		"backingClaimUID",
	); err != nil {
		return fmt.Errorf("set backing claim UID status: %w", err)
	}
	if err := unstructured.SetNestedField(
		updated.Object,
		devicePath,
		"status",
		"devicePath",
	); err != nil {
		return fmt.Errorf("set device path status: %w", err)
	}

	if _, err := controller.dynamic.Resource(contract.KataVolumeGVR).
		Namespace(updated.GetNamespace()).
		UpdateStatus(ctx, updated, metav1.UpdateOptions{}); err != nil {
		return fmt.Errorf("update KataVolume status: %w", err)
	}
	return nil
}
