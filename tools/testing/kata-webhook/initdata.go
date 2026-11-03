// Copyright (c) 2026 NVIDIA Corporation
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

	corev1 "k8s.io/api/core/v1"

	"github.com/pelletier/go-toml/v2"
)

const (
	initDataAnnotation = "io.katacontainers.config.hypervisor.cc_init_data"
	podSpecDataKey     = "pod-spec.json"
	initDataVersion    = "0.1.0"
	initDataAlgorithm  = "sha256"
)

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

// injectPodSpecIntoInitData adds the admission-time Kubernetes PodSpec to Kata
// init-data. If the Pod has no init-data annotation, it creates one without a
// policy.rego entry so the agent keeps using its image-bundled static policy.
func injectPodSpecIntoInitData(pod *corev1.Pod) (bool, error) {
	annotation, ok := pod.Annotations[initDataAnnotation]

	podSpec, err := json.Marshal(pod.Spec)
	if err != nil {
		return false, fmt.Errorf("serialize PodSpec: %w", err)
	}

	var data *initData
	if !ok || annotation == "" {
		data = &initData{
			Version:   initDataVersion,
			Algorithm: initDataAlgorithm,
			Data:      make(map[string]string),
		}
	} else {
		data, err = decodeInitData(annotation)
		if err != nil {
			return false, err
		}
	}
	if data.Data[podSpecDataKey] == string(podSpec) {
		return false, nil
	}

	data.Data[podSpecDataKey] = string(podSpec)
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
