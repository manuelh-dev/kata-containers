#!/usr/bin/env bash
#
# Copyright (c) 2026 NVIDIA Corporation
#
# SPDX-License-Identifier: Apache-2.0

set -o errexit
set -o nounset
set -o pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "${script_dir}/../../.." && pwd)

webhook_image=${WEBHOOK_IMAGE:-localhost/kata-initdata-webhook:prototype}
webhook_namespace=kata-initdata-webhook
workload_namespace=default
pod_name=cuda-vectoradd-kata-snp-policy-debug
runtime_class=kata-qemu-nvidia-gpu-snp-runtime-rs-devkit
artifact_root=${ARTIFACT_ROOT:-${script_dir}/artifacts}
pod_source=${POD_SOURCE:-${script_dir}/samples/nvidia-gpu-snp.yaml}
if [[ "${pod_source}" != /* ]]; then
	pod_source="${script_dir}/${pod_source}"
fi
pod_source_name=$(basename "${pod_source}")
pod_source_stem=${pod_source_name%.*}
pod_manifest=${artifact_root}/${pod_source_stem}-policy.yaml
artifact_dir=${ARTIFACT_DIR:-${artifact_root}/${pod_name}-$(date -u +%Y%m%dT%H%M%SZ)}
image_archive=${IMAGE_ARCHIVE:-/tmp/kata-initdata-webhook.tar}

kata_release=${KATA_RELEASE:-4.3.1}
tools_root=${KATA_TOOLS_ROOT:-/tmp/kata-tools-${kata_release}}
genpolicy=${GENPOLICY_BINARY:-${tools_root}/opt/kata/bin/genpolicy}
genpolicy_settings=${GENPOLICY_SETTINGS_DIR:-${tools_root}/opt/kata/share/defaults/kata-containers}
kata_ctl=${KATA_CTL:-/opt/kata/bin/kata-ctl}
guest_root=${GUEST_ROOT:-/real_root}
containerd_socket=${CONTAINERD_SOCKET:-/run/containerd/containerd.sock}
ctr=${CTR:-/usr/local/bin/ctr}
crictl=${CRICTL:-/usr/bin/crictl}

require() {
	command -v "$1" >/dev/null || {
		echo "ERROR: required command not found: $1" >&2
		exit 1
	}
}

prepare_tools() {
	require gh
	require tar

	if [[ -x "${genpolicy}" && -f "${genpolicy_settings}/rules.rego" ]]; then
		return
	fi

	mkdir -p "${tools_root}"
	gh release download "${kata_release}" \
		--repo kata-containers/kata-containers \
		--pattern "kata-tools-static-${kata_release}-amd64.tar.zst" \
		--dir "${tools_root}" \
		--clobber
	tar --zstd -xf \
		"${tools_root}/kata-tools-static-${kata_release}-amd64.tar.zst" \
		-C "${tools_root}"
}

build_webhook() {
	require podman
	require yq

	local go_version
	go_version=$(yq '.languages.golang.version' "${repo_root}/versions.yaml")

	podman build \
		--build-arg "GO_VERSION=${go_version}" \
		--tag "${webhook_image}" \
		"${script_dir}"
	rm -f "${image_archive}"
	podman save --output "${image_archive}" "${webhook_image}"

	# This recipe targets a single-node development cluster.
	sudo "${ctr}" --namespace k8s.io images import "${image_archive}"
}

deploy_webhook() {
	require envsubst
	require kubectl

	kubectl create namespace "${webhook_namespace}" \
		--dry-run=client -o yaml | kubectl apply -f -

	# This creates a serving certificate for
	# pod-annotate-webhook.kata-initdata-webhook.svc.
	(
		cd "${script_dir}"
		./create-certs.sh "${webhook_namespace}"
	)

	kubectl --namespace "${webhook_namespace}" apply \
		-f "${script_dir}/deploy/webhook-certs.yaml"

	local ca_bundle
	ca_bundle=$(
		awk '/^[[:space:]]*caBundle:/ { print $2; exit }' \
			"${script_dir}/deploy/webhook-registration.yaml"
	)
	if [[ -z "${ca_bundle}" ]]; then
		echo "ERROR: failed to read the generated CA bundle" >&2
		exit 1
	fi

	export CA_BUNDLE="${ca_bundle}"
	export WEBHOOK_IMAGE="${webhook_image}"
	export RUNTIME_CLASS="${runtime_class}"
	envsubst <"${script_dir}/deploy/initdata-webhook.yaml.tpl" |
		kubectl apply -f -

	kubectl label namespace "${workload_namespace}" \
		kata-containers.io/initdata-webhook=enabled --overwrite
	kubectl --namespace "${webhook_namespace}" rollout restart \
		deployment/kata-initdata-webhook
	kubectl --namespace "${webhook_namespace}" rollout status \
		deployment/kata-initdata-webhook --timeout=120s

	for _ in 1 2 3; do
		if kubectl create --dry-run=server -f - >/dev/null 2>&1 <<EOF
apiVersion: v1
kind: Pod
metadata:
  generateName: kata-initdata-webhook-check-
  namespace: ${workload_namespace}
spec:
  runtimeClassName: ${runtime_class}
  containers:
    - name: check
      image: registry.k8s.io/pause:3.10.2
EOF
		then
			return
		fi
	done

	echo "ERROR: webhook did not become reachable by the API server" >&2
	return 1
}

validate_pod_source() {
	require yq

	if [[ ! -f "${pod_source}" ]]; then
		echo "ERROR: Pod source manifest not found: ${pod_source}" >&2
		exit 1
	fi
	if [[ "$(yq eval-all '[select(.kind == "Pod")] | length' "${pod_source}")" != "1" ]]; then
		echo "ERROR: Pod source must contain one Pod object" >&2
		exit 1
	fi
	if [[ "$(yq 'select(.kind == "Pod") | .metadata.name' "${pod_source}")" != "${pod_name}" ]]; then
		echo "ERROR: source metadata.name must be ${pod_name}" >&2
		exit 1
	fi
	if [[ "$(yq 'select(.kind == "Pod") | .metadata.namespace' "${pod_source}")" != "${workload_namespace}" ]]; then
		echo "ERROR: source namespace must be ${workload_namespace}" >&2
		exit 1
	fi
	if [[ "$(yq 'select(.kind == "Pod") | .spec.runtimeClassName' "${pod_source}")" != "${runtime_class}" ]]; then
		echo "ERROR: source runtimeClassName must be ${runtime_class}" >&2
		exit 1
	fi
}

prepare_pod_manifest() {
	validate_pod_source
	mkdir -p "$(dirname "${pod_manifest}")"
	cp -- "${pod_source}" "${pod_manifest}"
}

generate_policy() {
	prepare_tools
	prepare_pod_manifest

	(
		cd "${tools_root}"
		sudo -E "${genpolicy}" \
			-u \
			-d="${containerd_socket}" \
			-y "${pod_manifest}" \
			-p "${genpolicy_settings}/rules.rego" \
			-j "${genpolicy_settings}" \
			--runtime-class-names "${runtime_class}"
	)

	if ! grep -q \
		'io.katacontainers.config.hypervisor.cc_init_data' \
		"${pod_manifest}"; then
		echo "ERROR: genpolicy did not add an init-data annotation" >&2
		exit 1
	fi

	echo "Source Pod manifest: ${pod_source}"
	echo "Generated policy manifest: ${pod_manifest}"
}

deploy_manifest() {
	local manifest=$1

	require kubectl

	kubectl --namespace "${workload_namespace}" delete pod "${pod_name}" \
		--ignore-not-found --wait=true
	kubectl apply -f "${manifest}"
	kubectl --namespace "${workload_namespace}" wait \
		--for=condition=Ready "pod/${pod_name}" --timeout=10m
}

deploy_pod() {
	if [[ ! -f "${pod_manifest}" ]]; then
		echo "ERROR: run '$0 generate-policy' first" >&2
		exit 1
	fi

	deploy_manifest "${pod_manifest}"
}

deploy_source_pod() {
	validate_pod_source
	deploy_manifest "${pod_source}"
}

inspect_guest() {
	require base64
	require gzip
	require kubectl
	require python3

	local sandbox_id
	sandbox_id=$(
		sudo "${crictl}" \
			--runtime-endpoint "unix://${containerd_socket}" \
			pods --name "${pod_name}" --state Ready -q |
			tail -n 1
	)
	if [[ -z "${sandbox_id}" ]]; then
		echo "ERROR: no sandbox found for ${pod_name}" >&2
		exit 1
	fi

	local -a config_paths
	mapfile -t config_paths < <(
		sudo "${kata_ctl}" exec "${sandbox_id}" -- \
			find "${guest_root}/run/kata-containers" \
			-mindepth 2 -maxdepth 2 -name config.json -print |
			tr -d '\r'
	)
	local -a cdi_paths
	mapfile -t cdi_paths < <(
		sudo "${kata_ctl}" exec "${sandbox_id}" -- \
			sh -c \
			'if [ -d "$1" ]; then find "$1" -maxdepth 1 -type f -print; fi' \
			sh "${guest_root}/var/run/cdi" |
			tr -d '\r'
	)

	local copied_root="${artifact_dir}/guest-root"
	local initdata_guest="${guest_root}/run/confidential-containers/initdata/initdata.toml"
	local initdata_host="${copied_root}/run/confidential-containers/initdata/initdata.toml"
	local image_store_guest="${guest_root}/run/kata-containers/image/meta_store.json"
	local image_store_host="${copied_root}/run/kata-containers/image/meta_store.json"
	local pod_spec_host="${artifact_dir}/pod-spec.json"

	# A fixed ARTIFACT_DIR may be reused. Remove only files produced by this
	# command so runtime-generated container ID directories cannot accumulate.
	rm -rf -- "${copied_root}" "${artifact_dir}/image-configs"
	rm -f -- \
		"${pod_spec_host}" \
		"${artifact_dir}/SHA256SUMS" \
		"${artifact_dir}/SHA256SUMS.tmp"
	mkdir -p "${artifact_dir}"

	copy_guest_file "${sandbox_id}" "${initdata_guest}" "${initdata_host}"
	copy_guest_file "${sandbox_id}" "${image_store_guest}" "${image_store_host}"

	local config_path
	for config_path in "${config_paths[@]}"; do
		copy_guest_file \
			"${sandbox_id}" \
			"${config_path}" \
			"${copied_root}${config_path#"${guest_root}"}"
	done
	local cdi_path
	for cdi_path in "${cdi_paths[@]}"; do
		copy_guest_file \
			"${sandbox_id}" \
			"${cdi_path}" \
			"${copied_root}${cdi_path#"${guest_root}"}"
	done

	python3 - "${initdata_host}" "${pod_spec_host}" <<'PY'
import pathlib
import sys
import tomllib

with open(sys.argv[1], "rb") as source:
    initdata = tomllib.load(source)
pathlib.Path(sys.argv[2]).write_bytes(
    initdata["data"]["pod-spec.json"].encode("utf-8")
)
PY

	python3 - "${image_store_host}" "${artifact_dir}/image-configs" <<'PY'
import json
import pathlib
import re
import sys

with open(sys.argv[1], encoding="utf-8") as source:
    image_store = json.load(source)

output_root = pathlib.Path(sys.argv[2])
for image_id, image_meta in sorted(image_store["image_db"].items()):
    output_dir = output_root / re.sub(r"[^A-Za-z0-9_.-]", "_", image_id)
    output_dir.mkdir(parents=True, exist_ok=True)
    (output_dir / "image-config.json").write_text(
        json.dumps(image_meta["image_config"], separators=(",", ":")),
        encoding="utf-8",
    )
    (output_dir / "image-meta.json").write_text(
        json.dumps(image_meta, separators=(",", ":")),
        encoding="utf-8",
    )
    (output_dir / "reference.txt").write_text(
        image_meta["reference"] + "\n",
        encoding="utf-8",
    )
PY

	echo "Sandbox: ${sandbox_id}"
	echo "Artifacts: ${artifact_dir}"
	echo
	echo "=== PodSpec from guest init-data"
	python3 -m json.tool "${pod_spec_host}"

	echo
	echo "=== Persisted effective OCI specs"
	for config_path in "${config_paths[@]}"; do
		local config_host="${copied_root}${config_path#"${guest_root}"}"
		echo
		echo "--- ${config_host}"
		python3 -m json.tool "${config_host}"
	done

	echo
	echo "=== Guest-pull image configurations"
	local image_config
	while IFS= read -r -d '' image_config; do
		echo
		echo "--- ${image_config}"
		python3 -m json.tool "${image_config}"
	done < <(find "${artifact_dir}/image-configs" -name image-config.json -print0)

	echo
	echo "=== Guest CDI specifications"
	local cdi_host
	for cdi_path in "${cdi_paths[@]}"; do
		cdi_host="${copied_root}${cdi_path#"${guest_root}"}"
		echo
		echo "--- ${cdi_host}"
		cat "${cdi_host}"
	done
}

copy_guest_file() {
	local sandbox_id=$1
	local guest_path=$2
	local host_path=$3
	local guest_digest
	local host_digest

	mkdir -p "$(dirname "${host_path}")"
	sudo "${kata_ctl}" exec "${sandbox_id}" -- \
		sh -c 'gzip -c "$1" | base64' sh "${guest_path}" |
		base64 --decode |
		gzip --decompress |
		tee "${host_path}" >/dev/null

	guest_digest=$(
		sudo "${kata_ctl}" exec "${sandbox_id}" -- sha256sum "${guest_path}" |
			awk '{ print $1 }'
	)
	host_digest=$(sha256sum "${host_path}" | awk '{ print $1 }')
	if [[ "${guest_digest}" != "${host_digest}" ]]; then
		echo "ERROR: copied file differs from guest: ${guest_path}" >&2
		return 1
	fi
}

cleanup() {
	kubectl --namespace "${workload_namespace}" delete pod "${pod_name}" \
		--ignore-not-found
	kubectl delete mutatingwebhookconfiguration kata-initdata-webhook \
		--ignore-not-found
	kubectl delete namespace "${webhook_namespace}" --ignore-not-found
	kubectl label namespace "${workload_namespace}" \
		kata-containers.io/initdata-webhook- >/dev/null 2>&1 || true
}

usage() {
	cat <<EOF
Usage: $0 COMMAND

Commands:
  prepare-tools    Download the genpolicy tools archive
  build            Build and import the webhook image into local containerd
  deploy-webhook   Create certificates and deploy the scoped webhook
  generate-policy  Copy POD_SOURCE and run genpolicy against the copy
  deploy-pod       Recreate the generated-policy Pod through the webhook
  deploy-source    Recreate POD_SOURCE directly through the webhook
  inspect          Export PodSpec, OCI specs, image metadata, and CDI specs
  all              Run every step above, ending with inspection
  direct-all       Skip genpolicy and run POD_SOURCE directly through the webhook
  cleanup          Remove the Pod and webhook deployment

Pod selection:
  POD_SOURCE        Input manifest; defaults to samples/nvidia-gpu-snp.yaml
EOF
}

case "${1:-}" in
prepare-tools)
	prepare_tools
	;;
build)
	build_webhook
	;;
deploy-webhook)
	deploy_webhook
	;;
generate-policy)
	generate_policy
	;;
deploy-pod)
	deploy_pod
	;;
deploy-source)
	deploy_source_pod
	;;
inspect)
	inspect_guest
	;;
all)
	prepare_tools
	build_webhook
	deploy_webhook
	generate_policy
	deploy_pod
	inspect_guest
	;;
direct-all)
	build_webhook
	deploy_webhook
	deploy_source_pod
	inspect_guest
	;;
cleanup)
	cleanup
	;;
*)
	usage
	exit 1
	;;
esac
