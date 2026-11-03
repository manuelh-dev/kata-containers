# Copyright (c) 2026 Microsoft Corporation
#
# SPDX-License-Identifier: Apache-2.0

IMAGE ?= quay.io/kata-containers/kata-webhook-example:latest
GO_VERSION ?= $(shell yq '.languages.golang.version' $(CURDIR)/../../../versions.yaml)
PROTOTYPE_SCRIPT := $(CURDIR)/initdata-podspec-prototype.sh

all: docker

docker:
	docker build \
	       -f $(CURDIR)/Dockerfile \
	       --build-arg GO_VERSION=$(GO_VERSION) \
	       -t $(IMAGE) \
	       $(CURDIR)

prototype-prepare-tools:
	$(PROTOTYPE_SCRIPT) prepare-tools

prototype-build:
	$(PROTOTYPE_SCRIPT) build

prototype-deploy-webhook:
	$(PROTOTYPE_SCRIPT) deploy-webhook

prototype-generate-policy:
	$(PROTOTYPE_SCRIPT) generate-policy

prototype-deploy-pod:
	$(PROTOTYPE_SCRIPT) deploy-pod

prototype-inspect:
	$(PROTOTYPE_SCRIPT) inspect

prototype-all:
	$(PROTOTYPE_SCRIPT) all

prototype-cleanup:
	$(PROTOTYPE_SCRIPT) cleanup

.PHONY: \
	all \
	docker \
	prototype-all \
	prototype-build \
	prototype-cleanup \
	prototype-deploy-pod \
	prototype-deploy-webhook \
	prototype-generate-policy \
	prototype-inspect \
	prototype-prepare-tools
