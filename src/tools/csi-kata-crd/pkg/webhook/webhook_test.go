// Copyright (c) 2026 Kata Containers contributors
//
// SPDX-License-Identifier: Apache-2.0

package webhook

import (
	"context"
	"encoding/json"
	"strings"
	"testing"

	"kata-containers/csi-kata-crd/pkg/contract"

	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"k8s.io/apimachinery/pkg/runtime"
	dynamicfake "k8s.io/client-go/dynamic/fake"
	kubernetesfake "k8s.io/client-go/kubernetes/fake"
)

func TestMutatePodInjectsBackingVolumeDevice(t *testing.T) {
	group := contract.APIGroup
	modeBlock := corev1.PersistentVolumeBlock
	modeFilesystem := corev1.PersistentVolumeFilesystem
	facadeClaim := &corev1.PersistentVolumeClaim{
		ObjectMeta: metav1.ObjectMeta{Name: "created", Namespace: "default"},
		Spec: corev1.PersistentVolumeClaimSpec{
			VolumeMode: &modeFilesystem,
			DataSourceRef: &corev1.TypedObjectReference{
				APIGroup: &group,
				Kind:     contract.KataVolumeKind,
				Name:     "created-kv",
			},
		},
	}
	backingClaim := &corev1.PersistentVolumeClaim{
		ObjectMeta: metav1.ObjectMeta{Name: "created-disk", Namespace: "default"},
		Spec: corev1.PersistentVolumeClaimSpec{
			VolumeMode: &modeBlock,
		},
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

	handler := New(
		kubernetesfake.NewSimpleClientset(facadeClaim, backingClaim),
		dynamicfake.NewSimpleDynamicClient(runtime.NewScheme(), kataVolume),
	)
	pod := corev1.Pod{
		ObjectMeta: metav1.ObjectMeta{Name: "application", Namespace: "default"},
		Spec: corev1.PodSpec{
			Containers: []corev1.Container{{
				Name: "application",
				VolumeMounts: []corev1.VolumeMount{{
					Name:      "data",
					MountPath: "/data",
				}},
			}},
			Volumes: []corev1.Volume{{
				Name: "data",
				VolumeSource: corev1.VolumeSource{
					PersistentVolumeClaim: &corev1.PersistentVolumeClaimVolumeSource{
						ClaimName: "created",
					},
				},
			}},
		},
	}
	raw, err := json.Marshal(pod)
	if err != nil {
		t.Fatal(err)
	}

	patch, err := handler.mutatePod(context.Background(), "default", raw)
	if err != nil {
		t.Fatal(err)
	}
	value := string(patch)
	for _, expected := range []string{
		"kata-backing-1234567812341234",
		"/dev/kata-crd-1234567812341234",
		"/spec/volumes/-",
		"/spec/containers/0/volumeDevices",
	} {
		if !strings.Contains(value, expected) {
			t.Errorf("patch does not contain %q: %s", expected, value)
		}
	}
}
