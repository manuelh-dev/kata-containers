// Copyright (c) NVIDIA Corporation
//
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"bytes"
	"compress/gzip"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"strings"

	corev1 "k8s.io/api/core/v1"

	"github.com/pelletier/go-toml/v2"
)

const (
	initDataAnnotation     = "io.katacontainers.config.hypervisor.cc_init_data"
	podSpecDataKey         = "pod-spec.json"
	podMetadataDataKey     = "pod-metadata.json"
	policyDataKey          = "policy.rego"
	ociSettingsDataKey     = "oci-settings.json"
	clusterSettingsDataKey = "cluster-settings.json"
	initDataVersion        = "0.1.0"
	initDataAlgorithm      = "sha256"
)

// injectionMode controls how the webhook handles an init-data entry that it
// injects from a file.
type injectionMode string

const (
	// injectionPreserve never sets or changes the entry.
	injectionPreserve injectionMode = "preserve"
	// injectionSetIfAbsent sets the entry only if init-data has none.
	injectionSetIfAbsent injectionMode = "set-if-absent"
	// injectionReplace always sets the entry, replacing any existing one.
	injectionReplace injectionMode = "replace"
	// injectionFailIfPresent rejects Pods whose init-data already has a
	// different entry, and sets it otherwise.
	injectionFailIfPresent injectionMode = "fail-if-present"
)

func parseInjectionMode(value string) (injectionMode, error) {
	switch mode := injectionMode(value); mode {
	case injectionPreserve, injectionSetIfAbsent, injectionReplace, injectionFailIfPresent:
		return mode, nil
	default:
		return "", fmt.Errorf("invalid mode %q", value)
	}
}

// initDataEntry is an init-data entry that the webhook injects from a file.
type initDataEntry struct {
	key     string
	mode    injectionMode
	content string
}

func loadInitDataEntry(key, modeValue, file string) (initDataEntry, error) {
	mode, err := parseInjectionMode(modeValue)
	if err != nil {
		return initDataEntry{}, fmt.Errorf("%s: %w", key, err)
	}
	if mode == injectionPreserve {
		return initDataEntry{key: key, mode: mode}, nil
	}
	if file == "" {
		return initDataEntry{}, fmt.Errorf("%s: mode %q requires a file", key, mode)
	}

	content, err := os.ReadFile(file)
	if err != nil {
		return initDataEntry{}, fmt.Errorf("%s: %w", key, err)
	}
	if len(bytes.TrimSpace(content)) == 0 {
		return initDataEntry{}, fmt.Errorf("%s: file %s is empty", key, file)
	}
	if strings.HasSuffix(key, ".json") && !json.Valid(content) {
		return initDataEntry{}, fmt.Errorf("%s: file %s is not valid JSON", key, file)
	}

	return initDataEntry{key: key, mode: mode, content: string(content)}, nil
}

// apply updates the entry in data according to the mode, and reports whether
// data changed.
func (e initDataEntry) apply(data map[string]string) (bool, error) {
	existing, present := data[e.key]
	switch e.mode {
	case injectionSetIfAbsent:
		if present {
			return false, nil
		}
	case injectionReplace:
	case injectionFailIfPresent:
		// The webhook may be reinvoked on a Pod it already mutated.
		if present && existing != e.content {
			return false, fmt.Errorf("init-data already contains %s", e.key)
		}
	default:
		return false, nil
	}

	return setDataEntry(data, e.key, e.content), nil
}

// podMetadata is the part of the Pod's ObjectMeta that the guest needs next to
// the PodSpec. Other ObjectMeta fields are either not yet assigned at admission
// time, such as uid, or volatile, such as managedFields and
// kubectl.kubernetes.io/last-applied-configuration.
type podMetadata struct {
	Name         string `json:"name"`
	GenerateName string `json:"generateName"`
	Namespace    string `json:"namespace"`
}

type initData struct {
	Version   string            `toml:"version"`
	Algorithm string            `toml:"algorithm"`
	Data      map[string]string `toml:"data"`
}

func decodeInitData(annotation string) (*initData, error) {
	compressed, err := base64.StdEncoding.DecodeString(annotation)
	if err != nil {
		return nil, fmt.Errorf("decode init-data base64: %w", err)
	}

	reader, err := gzip.NewReader(bytes.NewReader(compressed))
	if err != nil {
		return nil, fmt.Errorf("open init-data gzip stream: %w", err)
	}
	defer reader.Close()

	raw, err := io.ReadAll(reader)
	if err != nil {
		return nil, fmt.Errorf("decompress init-data: %w", err)
	}

	var data initData
	if err := toml.Unmarshal(raw, &data); err != nil {
		return nil, fmt.Errorf("parse init-data TOML: %w", err)
	}
	if data.Data == nil {
		data.Data = make(map[string]string)
	}

	return &data, nil
}

func encodeInitData(data *initData) (string, error) {
	raw, err := toml.Marshal(data)
	if err != nil {
		return "", fmt.Errorf("serialize init-data TOML: %w", err)
	}

	var compressed bytes.Buffer
	writer := gzip.NewWriter(&compressed)
	if _, err := writer.Write(raw); err != nil {
		return "", fmt.Errorf("compress init-data: %w", err)
	}
	if err := writer.Close(); err != nil {
		return "", fmt.Errorf("finish init-data compression: %w", err)
	}

	return base64.StdEncoding.EncodeToString(compressed.Bytes()), nil
}

func setDataEntry(data map[string]string, key, value string) bool {
	if existing, ok := data[key]; ok && existing == value {
		return false
	}
	data[key] = value
	return true
}

// injectIntoInitData adds the admission-time Kubernetes PodSpec and the Pod's
// name, generateName, and namespace to Kata init-data, creating the init-data
// annotation if needed, and applies entries, such as policy.rego, according to
// their modes. Without a policy.rego entry, the agent keeps using its
// image-bundled default policy. namespace must come from the admission
// request, because the Pod object does not always carry it.
func injectIntoInitData(pod *corev1.Pod, namespace string, entries []initDataEntry) (bool, error) {
	podSpec, err := json.Marshal(pod.Spec)
	if err != nil {
		return false, fmt.Errorf("serialize PodSpec: %w", err)
	}
	metadata, err := json.Marshal(podMetadata{
		Name:         pod.Name,
		GenerateName: pod.GenerateName,
		Namespace:    namespace,
	})
	if err != nil {
		return false, fmt.Errorf("serialize Pod metadata: %w", err)
	}

	data := &initData{
		Version:   initDataVersion,
		Algorithm: initDataAlgorithm,
		Data:      make(map[string]string),
	}
	if annotation := pod.Annotations[initDataAnnotation]; annotation != "" {
		decoded, err := decodeInitData(annotation)
		if err != nil {
			return false, err
		}
		data = decoded
	}

	changed := false
	for _, entry := range entries {
		entryChanged, err := entry.apply(data.Data)
		if err != nil {
			return false, err
		}
		changed = entryChanged || changed
	}
	changed = setDataEntry(data.Data, podSpecDataKey, string(podSpec)) || changed
	changed = setDataEntry(data.Data, podMetadataDataKey, string(metadata)) || changed
	if !changed {
		return false, nil
	}

	encoded, err := encodeInitData(data)
	if err != nil {
		return false, err
	}

	if pod.Annotations == nil {
		pod.Annotations = make(map[string]string)
	}
	pod.Annotations[initDataAnnotation] = encoded
	return true, nil
}
