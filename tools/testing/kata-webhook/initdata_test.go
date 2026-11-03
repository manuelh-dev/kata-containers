// Copyright (c) 2026 NVIDIA Corporation
//
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"encoding/json"
	"testing"

	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
)

func TestInjectPodSpecIntoInitData(t *testing.T) {
	annotation, err := encodeInitData(&initData{
		Version:   "0.1.0",
		Algorithm: "sha256",
		Data: map[string]string{
			"policy.rego": "package agent_policy\n",
		},
	})
	if err != nil {
		t.Fatal(err)
	}

	runtimeClass := "kata-qemu"
	pod := &corev1.Pod{
		ObjectMeta: metav1.ObjectMeta{
			Annotations: map[string]string{initDataAnnotation: annotation},
		},
		Spec: corev1.PodSpec{
			RuntimeClassName: &runtimeClass,
			Containers: []corev1.Container{{
				Name:    "workload",
				Image:   "example.com/workload:latest",
				Command: []string{"/bin/workload"},
			}},
		},
	}

	changed, err := injectPodSpecIntoInitData(pod)
	if err != nil {
		t.Fatal(err)
	}
	if !changed {
		t.Fatal("expected init-data to change")
	}

	data, err := decodeInitData(pod.Annotations[initDataAnnotation])
	if err != nil {
		t.Fatal(err)
	}
	if data.Data["policy.rego"] != "package agent_policy\n" {
		t.Fatal("existing policy was not preserved")
	}

	expected, err := json.Marshal(pod.Spec)
	if err != nil {
		t.Fatal(err)
	}
	if data.Data[podSpecDataKey] != string(expected) {
		t.Fatalf("unexpected PodSpec:\ngot:  %s\nwant: %s", data.Data[podSpecDataKey], expected)
	}

	encoded := pod.Annotations[initDataAnnotation]
	changed, err = injectPodSpecIntoInitData(pod)
	if err != nil {
		t.Fatal(err)
	}
	if changed {
		t.Fatal("expected a second injection to be idempotent")
	}
	if pod.Annotations[initDataAnnotation] != encoded {
		t.Fatal("idempotent injection changed the annotation")
	}
}

func TestInjectPodSpecIntoInitDataCreatesAnnotation(t *testing.T) {
	pod := &corev1.Pod{
		Spec: corev1.PodSpec{
			Containers: []corev1.Container{{
				Name:  "workload",
				Image: "example.com/workload:latest",
			}},
		},
	}

	changed, err := injectPodSpecIntoInitData(pod)
	if err != nil {
		t.Fatal(err)
	}
	if !changed {
		t.Fatal("expected init-data to be created")
	}

	data, err := decodeInitData(pod.Annotations[initDataAnnotation])
	if err != nil {
		t.Fatal(err)
	}
	if data.Version != initDataVersion {
		t.Fatalf("unexpected init-data version: %q", data.Version)
	}
	if data.Algorithm != initDataAlgorithm {
		t.Fatalf("unexpected init-data algorithm: %q", data.Algorithm)
	}
	if _, ok := data.Data["policy.rego"]; ok {
		t.Fatal("generated init-data unexpectedly contains a policy")
	}

	expected, err := json.Marshal(pod.Spec)
	if err != nil {
		t.Fatal(err)
	}
	if data.Data[podSpecDataKey] != string(expected) {
		t.Fatalf("unexpected PodSpec:\ngot:  %s\nwant: %s", data.Data[podSpecDataKey], expected)
	}
}

func TestInjectPodSpecIntoMalformedInitData(t *testing.T) {
	pod := &corev1.Pod{
		ObjectMeta: metav1.ObjectMeta{
			Annotations: map[string]string{initDataAnnotation: "not-base64"},
		},
	}

	if _, err := injectPodSpecIntoInitData(pod); err == nil {
		t.Fatal("expected malformed init-data to fail")
	}
}
