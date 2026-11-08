// Copyright (c) 2026 Kata Containers contributors
//
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"context"
	"flag"
	"log"
	"os/signal"
	"syscall"
	"time"

	"kata-containers/csi-kata-crd/pkg/controller"

	"k8s.io/client-go/dynamic"
	"k8s.io/client-go/kubernetes"
	"k8s.io/client-go/rest"
)

func main() {
	var interval time.Duration
	flag.DurationVar(&interval, "reconcile-interval", 2*time.Second, "reconciliation interval")
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

	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()

	if err := controller.New(kube, dynamicClient, interval).Run(ctx); err != nil {
		log.Fatalf("run controller: %v", err)
	}
}
