#!/usr/bin/env bash
#
# Copyright (c) NVIDIA Corporation
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
initdata_runtime_classes=${INITDATA_RUNTIME_CLASSES:-kata-qemu-nvidia-gpu-snp-runtime-rs-devkit}
IFS=',' read -r -a initdata_runtime_class_list <<<"${initdata_runtime_classes}"
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
agent_policy_mode=${AGENT_POLICY_MODE:-preserve}
agent_policy_file=${AGENT_POLICY_FILE:-${script_dir}/policy/static-policy.rego}
oci_settings_file=${OCI_SETTINGS_FILE:-}
cluster_settings_file=${CLUSTER_SETTINGS_FILE:-}

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

validate_initdata_runtime_classes() {
	if [[ "${#initdata_runtime_class_list[@]}" -eq 0 ]]; then
		echo "ERROR: INITDATA_RUNTIME_CLASSES must name at least one RuntimeClass" >&2
		exit 1
	fi

	# The names are embedded in a CEL expression, so only allow valid
	# RuntimeClass names (DNS subdomains).
	local runtime_class
	for runtime_class in "${initdata_runtime_class_list[@]}"; do
		if [[ ! "${runtime_class}" =~ ^[a-z0-9]([-a-z0-9]*[a-z0-9])?(\.[a-z0-9]([-a-z0-9]*[a-z0-9])?)*$ ]]; then
			echo "ERROR: invalid RuntimeClass name in INITDATA_RUNTIME_CLASSES: '${runtime_class}'" >&2
			exit 1
		fi
	done
}

is_initdata_runtime_class() {
	local runtime_class
	for runtime_class in "${initdata_runtime_class_list[@]}"; do
		if [[ "${runtime_class}" == "$1" ]]; then
			return 0
		fi
	done
	return 1
}

validate_settings_source() {
	local file=$1

	if [[ -z "${file}" ]]; then
		return
	fi
	if ! python3 -m json.tool "${file}" >/dev/null; then
		echo "ERROR: ${file} is not a valid JSON file" >&2
		exit 1
	fi
}

apply_settings_configmap() {
	local name=$1
	local key=$2
	local file=$3

	# Without a file, delete the ConfigMap so a previous deployment's
	# settings cannot remain mounted.
	if [[ -z "${file}" ]]; then
		kubectl --namespace "${webhook_namespace}" delete configmap \
			"${name}" --ignore-not-found
		return
	fi
	kubectl --namespace "${webhook_namespace}" create configmap "${name}" \
		--from-file="${key}=${file}" \
		--dry-run=client -o yaml | kubectl apply -f -
}

source_runtime_class() {
	yq 'select(.kind == "Pod") | .spec.runtimeClassName' "${pod_source}"
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
	require python3
	validate_initdata_runtime_classes
	validate_settings_source "${oci_settings_file}"
	validate_settings_source "${cluster_settings_file}"

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

	kubectl --namespace "${webhook_namespace}" create configmap \
		kata-initdata-webhook-agent-policy \
		--from-file=policy.rego="${agent_policy_file}" \
		--dry-run=client -o yaml | kubectl apply -f -
	apply_settings_configmap kata-initdata-webhook-oci-settings \
		oci-settings.json "${oci_settings_file}"
	apply_settings_configmap kata-initdata-webhook-cluster-settings \
		cluster-settings.json "${cluster_settings_file}"

	local ca_bundle
	ca_bundle=$(
		awk '/^[[:space:]]*caBundle:/ { print $2; exit }' \
			"${script_dir}/deploy/webhook-registration.yaml"
	)
	if [[ -z "${ca_bundle}" ]]; then
		echo "ERROR: failed to read the generated CA bundle" >&2
		exit 1
	fi

	local runtime_class
	local runtime_classes_cel=""
	for runtime_class in "${initdata_runtime_class_list[@]}"; do
		runtime_classes_cel+="${runtime_classes_cel:+, }'${runtime_class}'"
	done

	export CA_BUNDLE="${ca_bundle}"
	export WEBHOOK_IMAGE="${webhook_image}"
	export INITDATA_RUNTIME_CLASSES
	INITDATA_RUNTIME_CLASSES=$(IFS=','; echo "${initdata_runtime_class_list[*]}")
	export INITDATA_RUNTIME_CLASSES_CEL="[${runtime_classes_cel}]"
	export AGENT_POLICY_MODE="${agent_policy_mode}"
	# An empty flag value tells the webhook that no settings file was given.
	export WEBHOOK_OCI_SETTINGS_FILE=${oci_settings_file:+/etc/webhook/oci-settings/oci-settings.json}
	export WEBHOOK_CLUSTER_SETTINGS_FILE=${cluster_settings_file:+/etc/webhook/cluster-settings/cluster-settings.json}
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
  runtimeClassName: ${initdata_runtime_class_list[0]}
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
	validate_initdata_runtime_classes
	if ! is_initdata_runtime_class "$(source_runtime_class)"; then
		echo "ERROR: source runtimeClassName must be one of: ${initdata_runtime_classes}" >&2
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

	local runtime_class
	runtime_class=$(source_runtime_class)
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
	local pod_metadata_host="${artifact_dir}/pod-metadata.json"
	local policy_host="${artifact_dir}/policy.rego"
	local -a initdata_entries=(
		pod-spec.json
		pod-metadata.json
		policy.rego
		oci-settings.json
		cluster-settings.json
	)

	# A fixed ARTIFACT_DIR may be reused. Remove only files produced by this
	# command so runtime-generated container ID directories cannot accumulate.
	rm -rf -- "${copied_root}" "${artifact_dir}/image-configs"
	local entry
	for entry in "${initdata_entries[@]}"; do
		rm -f -- "${artifact_dir}/${entry}"
	done
	rm -f -- \
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

	python3 - "${initdata_host}" "${artifact_dir}" "${initdata_entries[@]}" <<'PY'
import pathlib
import sys
import tomllib

initdata_path, output_dir, *entries = sys.argv[1:]
with open(initdata_path, "rb") as source:
    data = tomllib.load(source)["data"]
if "pod-spec.json" not in data:
    sys.exit("ERROR: guest init-data has no pod-spec.json")
for entry in entries:
    if entry in data:
        (pathlib.Path(output_dir) / entry).write_bytes(data[entry].encode("utf-8"))
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
	if [[ -f "${pod_metadata_host}" ]]; then
		echo "=== Pod metadata from guest init-data"
		python3 -m json.tool "${pod_metadata_host}"
	else
		echo "=== No Pod metadata in guest init-data"
	fi

	echo
	if [[ -f "${policy_host}" ]]; then
		echo "=== Agent policy from guest init-data: ${policy_host}"
		echo "sha256: $(sha256sum "${policy_host}" | awk '{ print $1 }')"
	else
		echo "=== No agent policy in guest init-data"
	fi

	local settings_host
	for entry in oci-settings.json cluster-settings.json; do
		settings_host="${artifact_dir}/${entry}"
		echo
		if [[ -f "${settings_host}" ]]; then
			echo "=== ${entry} from guest init-data: ${settings_host}"
			echo "sha256: $(sha256sum "${settings_host}" | awk '{ print $1 }')"
			python3 -m json.tool "${settings_host}"
		else
			echo "=== No ${entry} in guest init-data"
		fi
	done

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
  inspect          Export init-data entries, OCI specs, image metadata, and CDI specs
  all              Run every step above, ending with inspection
  direct-all       Skip genpolicy and run POD_SOURCE directly through the webhook
  cleanup          Remove the Pod and webhook deployment

Pod selection:
  POD_SOURCE        Input manifest; defaults to samples/nvidia-gpu-snp.yaml
  INITDATA_RUNTIME_CLASSES
                    Comma-separated RuntimeClasses whose Pods receive init-data;
                    defaults to kata-qemu-nvidia-gpu-snp-runtime-rs-devkit

Agent policy injection (deploy-webhook):
  AGENT_POLICY_MODE preserve (default), set-if-absent, replace or fail-if-present
  AGENT_POLICY_FILE Policy to inject; defaults to policy/static-policy.rego

OCI and cluster settings injection (deploy-webhook):
  OCI_SETTINGS_FILE      JSON file that always replaces oci-settings.json; no default
  CLUSTER_SETTINGS_FILE  JSON file that always replaces cluster-settings.json; no default
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
