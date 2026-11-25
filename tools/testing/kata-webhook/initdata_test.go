// Copyright (c) NVIDIA Corporation
//
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"encoding/json"
	"os"
	"path/filepath"
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

	changed, err := injectIntoInitData(pod, "default", nil)
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
	changed, err = injectIntoInitData(pod, "default", nil)
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

	changed, err := injectIntoInitData(pod, "default", nil)
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

func TestInjectPodMetadataIntoInitData(t *testing.T) {
	tests := []struct {
		name         string
		podName      string
		generateName string
		want         string
	}{
		{
			name:    "named Pod",
			podName: "workload",
			want:    `{"name":"workload","generateName":"","namespace":"request-namespace"}`,
		},
		{
			name:         "generated name",
			generateName: "workload-",
			want:         `{"name":"","generateName":"workload-","namespace":"request-namespace"}`,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			pod := &corev1.Pod{
				ObjectMeta: metav1.ObjectMeta{
					Name:         tt.podName,
					GenerateName: tt.generateName,
					Namespace:    "object-namespace",
					Labels:       map[string]string{"app": "workload"},
					Annotations: map[string]string{
						"kubectl.kubernetes.io/last-applied-configuration": `{"kind":"Pod"}`,
					},
					ManagedFields: []metav1.ManagedFieldsEntry{{Manager: "kubectl", Operation: metav1.ManagedFieldsOperationUpdate}},
				},
				Spec: corev1.PodSpec{
					Containers: []corev1.Container{{Name: "workload", Image: "example.com/workload:latest"}},
				},
			}

			changed, err := injectIntoInitData(pod, "request-namespace", nil)
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
			if got := data.Data[podMetadataDataKey]; got != tt.want {
				t.Fatalf("unexpected Pod metadata:\ngot:  %s\nwant: %s", got, tt.want)
			}

			encoded := pod.Annotations[initDataAnnotation]
			changed, err = injectIntoInitData(pod, "request-namespace", nil)
			if err != nil {
				t.Fatal(err)
			}
			if changed || pod.Annotations[initDataAnnotation] != encoded {
				t.Fatal("expected a second injection to be idempotent")
			}
		})
	}
}

func TestInjectAgentPolicyIntoInitData(t *testing.T) {
	const (
		existingPolicy = "package agent_policy\n# generated\n"
		staticPolicy   = "package agent_policy\n# static\n"
	)

	tests := []struct {
		name           string
		mode           injectionMode
		existingPolicy *string
		wantPolicy     *string
		wantErr        bool
	}{
		{name: "preserve keeps existing policy", mode: injectionPreserve, existingPolicy: ptr(existingPolicy), wantPolicy: ptr(existingPolicy)},
		{name: "preserve does not add policy", mode: injectionPreserve},
		{name: "set-if-absent keeps existing policy", mode: injectionSetIfAbsent, existingPolicy: ptr(existingPolicy), wantPolicy: ptr(existingPolicy)},
		{name: "set-if-absent adds policy", mode: injectionSetIfAbsent, wantPolicy: ptr(staticPolicy)},
		{name: "replace replaces existing policy", mode: injectionReplace, existingPolicy: ptr(existingPolicy), wantPolicy: ptr(staticPolicy)},
		{name: "replace adds policy", mode: injectionReplace, wantPolicy: ptr(staticPolicy)},
		{name: "fail-if-present rejects existing policy", mode: injectionFailIfPresent, existingPolicy: ptr(existingPolicy), wantErr: true},
		{name: "fail-if-present accepts its own policy", mode: injectionFailIfPresent, existingPolicy: ptr(staticPolicy), wantPolicy: ptr(staticPolicy)},
		{name: "fail-if-present adds policy", mode: injectionFailIfPresent, wantPolicy: ptr(staticPolicy)},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			existing := map[string]string{}
			if tt.existingPolicy != nil {
				existing[policyDataKey] = *tt.existingPolicy
			}
			annotation, err := encodeInitData(&initData{
				Version:   initDataVersion,
				Algorithm: initDataAlgorithm,
				Data:      existing,
			})
			if err != nil {
				t.Fatal(err)
			}
			pod := &corev1.Pod{
				ObjectMeta: metav1.ObjectMeta{
					Annotations: map[string]string{initDataAnnotation: annotation},
				},
				Spec: corev1.PodSpec{
					Containers: []corev1.Container{{Name: "workload", Image: "example.com/workload:latest"}},
				},
			}

			entries := []initDataEntry{{key: policyDataKey, mode: tt.mode, content: staticPolicy}}
			_, err = injectIntoInitData(pod, "default", entries)
			if (err != nil) != tt.wantErr {
				t.Fatalf("expected error: %v, got: %v", tt.wantErr, err)
			}
			if err != nil {
				return
			}

			data, err := decodeInitData(pod.Annotations[initDataAnnotation])
			if err != nil {
				t.Fatal(err)
			}
			policy, ok := data.Data[policyDataKey]
			if tt.wantPolicy == nil {
				if ok {
					t.Fatalf("unexpected policy: %q", policy)
				}
			} else if policy != *tt.wantPolicy {
				t.Fatalf("unexpected policy:\ngot:  %q\nwant: %q", policy, *tt.wantPolicy)
			}
			if data.Data[podSpecDataKey] == "" {
				t.Fatal("PodSpec was not added to init-data")
			}

			encoded := pod.Annotations[initDataAnnotation]
			changed, err := injectIntoInitData(pod, "default", entries)
			if err != nil {
				t.Fatal(err)
			}
			if changed || pod.Annotations[initDataAnnotation] != encoded {
				t.Fatal("expected a second injection to be idempotent")
			}
		})
	}
}

func TestInjectSettingsIntoInitData(t *testing.T) {
	const (
		ociSettings = `{"1.2.0":{"namespaces":["ipc","uts","mount"]}}`
	)

	annotation, err := encodeInitData(&initData{
		Version:   initDataVersion,
		Algorithm: initDataAlgorithm,
		Data:      map[string]string{ociSettingsDataKey: `{"provided":"by the Pod"}`},
	})
	if err != nil {
		t.Fatal(err)
	}
	pod := &corev1.Pod{
		ObjectMeta: metav1.ObjectMeta{
			Annotations: map[string]string{initDataAnnotation: annotation},
		},
		Spec: corev1.PodSpec{
			Containers: []corev1.Container{{Name: "workload", Image: "example.com/workload:latest"}},
		},
	}
	entries := []initDataEntry{
		{key: policyDataKey, mode: injectionPreserve},
		{key: ociSettingsDataKey, mode: injectionReplace, content: ociSettings},
	}

	changed, err := injectIntoInitData(pod, "default", entries)
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
	if got := data.Data[ociSettingsDataKey]; got != ociSettings {
		t.Fatalf("unexpected OCI settings:\ngot:  %s\nwant: %s", got, ociSettings)
	}
	if _, ok := data.Data[policyDataKey]; ok {
		t.Fatal("preserve mode unexpectedly added a policy")
	}

	encoded := pod.Annotations[initDataAnnotation]
	changed, err = injectIntoInitData(pod, "default", entries)
	if err != nil {
		t.Fatal(err)
	}
	if changed || pod.Annotations[initDataAnnotation] != encoded {
		t.Fatal("expected a second injection to be idempotent")
	}
}

func TestLoadInitDataEntry(t *testing.T) {
	writeFile := func(name, content string) string {
		path := filepath.Join(t.TempDir(), name)
		if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
			t.Fatal(err)
		}
		return path
	}
	policyFile := writeFile("policy.rego", "package agent_policy\n")
	emptyFile := writeFile("empty.rego", " \n")
	settingsFile := writeFile("settings.json", `{"services":[]}`)
	invalidSettingsFile := writeFile("invalid.json", `{"services":`)

	tests := []struct {
		name    string
		key     string
		mode    string
		file    string
		wantErr bool
	}{
		{name: "preserve needs no file", key: policyDataKey, mode: "preserve"},
		{name: "set-if-absent with file", key: policyDataKey, mode: "set-if-absent", file: policyFile},
		{name: "unknown mode", key: policyDataKey, mode: "merge", file: policyFile, wantErr: true},
		{name: "replace without file", key: policyDataKey, mode: "replace", wantErr: true},
		{name: "missing file", key: policyDataKey, mode: "replace", file: filepath.Join(t.TempDir(), "missing.rego"), wantErr: true},
		{name: "empty file", key: policyDataKey, mode: "fail-if-present", file: emptyFile, wantErr: true},
		{name: "valid JSON", key: ociSettingsDataKey, mode: "replace", file: settingsFile},
		{name: "invalid JSON", key: ociSettingsDataKey, mode: "replace", file: invalidSettingsFile, wantErr: true},
		{name: "JSON check only for JSON entries", key: policyDataKey, mode: "replace", file: invalidSettingsFile},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, err := loadInitDataEntry(tt.key, tt.mode, tt.file)
			if (err != nil) != tt.wantErr {
				t.Fatalf("expected error: %v, got: %v", tt.wantErr, err)
			}
		})
	}
}

func TestInjectIntoMalformedInitData(t *testing.T) {
	pod := &corev1.Pod{
		ObjectMeta: metav1.ObjectMeta{
			Annotations: map[string]string{initDataAnnotation: "not-base64"},
		},
	}

	if _, err := injectIntoInitData(pod, "default", testPolicyEntries); err == nil {
		t.Fatal("expected malformed init-data to fail")
	}
}
