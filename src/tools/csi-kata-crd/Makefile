#
# Copyright (c) 2026 Kata Containers contributors
#
# SPDX-License-Identifier: Apache-2.0
#

GO ?= go
IMAGE ?= localhost/csi-kata-crd:prototype

BIN_DIR := bin
BINARIES := csi-kata-crd-controller csi-kata-crd-webhook csi-kata-crd-node

.PHONY: all build check image test clean

all: build

build: $(addprefix $(BIN_DIR)/,$(BINARIES))

$(BIN_DIR):
	mkdir -p $@

$(BIN_DIR)/%: | $(BIN_DIR)
	CGO_ENABLED=0 $(GO) build -o $@ ./cmd/$*

check:
	test -z "$$($(GO) fmt ./...)"
	$(GO) vet ./...

test:
	$(GO) test ./...

image:
	podman build -t $(IMAGE) .

clean:
	rm -rf $(BIN_DIR)
