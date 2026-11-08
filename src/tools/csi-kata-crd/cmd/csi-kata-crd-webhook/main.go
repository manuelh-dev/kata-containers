// Copyright (c) 2026 Kata Containers contributors
//
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"flag"
	"log"
	"net/http"

	"kata-containers/csi-kata-crd/pkg/webhook"

	"k8s.io/client-go/dynamic"
	"k8s.io/client-go/kubernetes"
	"k8s.io/client-go/rest"
)

func main() {
	var address string
	var certificateFile string
	var keyFile string
	flag.StringVar(&address, "listen", ":8443", "HTTPS listen address")
	flag.StringVar(&certificateFile, "tls-cert-file", "/tls/tls.crt", "TLS certificate")
	flag.StringVar(&keyFile, "tls-key-file", "/tls/tls.key", "TLS private key")
	flag.Parse()

	config, err := rest.InClusterConfig()
	if err != nil {
		log.Fatalf("load in-cluster configuration: %v", err)
	}
	kube, err := kubernetes.NewForConfig(config)
	if err != nil {
		log.Fatalf("create Kubernetes client: %v", err)
	}
	dynamicClient, err := dynamic.NewForConfig(config)
	if err != nil {
		log.Fatalf("create dynamic Kubernetes client: %v", err)
	}

	server := &http.Server{
		Addr:    address,
		Handler: webhook.New(kube, dynamicClient),
	}
	log.Printf("listening on %s", address)
	if err := server.ListenAndServeTLS(certificateFile, keyFile); err != nil {
		log.Fatalf("serve webhook: %v", err)
	}
}
