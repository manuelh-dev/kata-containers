// Copyright (c) NVIDIA Corporation
//
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"os"
	"path/filepath"
	"testing"

	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
)

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
			_, err = injectIntoInitData(pod, entries)
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

			encoded := pod.Annotations[initDataAnnotation]
			changed, err := injectIntoInitData(pod, entries)
			if err != nil {
				t.Fatal(err)
			}
			if changed || pod.Annotations[initDataAnnotation] != encoded {
				t.Fatal("expected a second injection to be idempotent")
			}
		})
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

	if _, err := injectIntoInitData(pod, testPolicyEntries); err == nil {
		t.Fatal("expected malformed init-data to fail")
	}
}
