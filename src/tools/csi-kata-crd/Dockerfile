# Copyright (c) 2026 Kata Containers contributors
#
# SPDX-License-Identifier: Apache-2.0

FROM golang:1.26.10-alpine AS build

WORKDIR /src
COPY . .
RUN CGO_ENABLED=0 go build -o /out/csi-kata-crd-controller ./cmd/csi-kata-crd-controller && \
    CGO_ENABLED=0 go build -o /out/csi-kata-crd-webhook ./cmd/csi-kata-crd-webhook && \
    CGO_ENABLED=0 go build -o /out/csi-kata-crd-node ./cmd/csi-kata-crd-node

FROM alpine:3.23

RUN apk add --no-cache ca-certificates
COPY --from=build /out/ /
