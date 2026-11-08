// Copyright (c) 2026 Kata Containers contributors
//
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"flag"
	"log"

	katacsi "kata-containers/csi-kata-crd/pkg/csi"
)

func main() {
	var endpoint string
	var nodeID string
	var directVolumeRoot string
	flag.StringVar(&endpoint, "endpoint", "unix:///csi/csi.sock", "CSI endpoint")
	flag.StringVar(&nodeID, "node-id", "", "Kubernetes node name")
	flag.StringVar(
		&directVolumeRoot,
		"direct-volume-root",
		"/run/kata-containers/shared/direct-volumes",
		"DAV metadata root",
	)
	flag.Parse()

	if nodeID == "" {
		log.Fatal("node-id is required")
	}
	if err := katacsi.New(nodeID, directVolumeRoot).Run(endpoint); err != nil {
		log.Fatalf("run CSI node service: %v", err)
	}
}
