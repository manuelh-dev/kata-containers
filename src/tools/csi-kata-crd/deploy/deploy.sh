#!/usr/bin/env bash
#
# Copyright (c) 2026 Kata Containers contributors
#
# SPDX-License-Identifier: Apache-2.0

set -o errexit
set -o nounset
set -o pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

service_name="csi-kata-crd-webhook"
namespace="kata-crd"
dns_name="${service_name}.${namespace}.svc"

openssl req -x509 -nodes -newkey rsa:2048 \
	-keyout "${tmp_dir}/tls.key" \
	-out "${tmp_dir}/tls.crt" \
	-days 2 \
	-subj "/CN=${dns_name}" \
	-addext "subjectAltName=DNS:${service_name},DNS:${service_name}.${namespace},DNS:${dns_name}"

kubectl apply -f "${script_dir}/crd.yaml"
kubectl apply -f "${script_dir}/rbac.yaml"
kubectl -n "${namespace}" create secret tls csi-kata-crd-webhook-tls \
	--cert="${tmp_dir}/tls.crt" \
	--key="${tmp_dir}/tls.key" \
	--dry-run=client \
	-o yaml | kubectl apply -f -

kubectl apply -f "${script_dir}/controller.yaml"
kubectl apply -f "${script_dir}/webhook.yaml"
kubectl apply -f "${script_dir}/csi-node.yaml"
kubectl apply -f "${script_dir}/storageclass.yaml"

CA_BUNDLE="$(base64 -w 0 < "${tmp_dir}/tls.crt")" \
	envsubst < "${script_dir}/webhook-registration.yaml.in" |
	kubectl apply -f -
