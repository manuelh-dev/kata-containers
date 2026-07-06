#!/usr/bin/env bats
#
# Copyright (c) 2026 NVIDIA Corporation
#
# SPDX-License-Identifier: Apache-2.0

# shellcheck disable=SC2154

load "${BATS_TEST_DIRNAME}/lib.sh"
load "${BATS_TEST_DIRNAME}/../../common.bash"
load "${BATS_TEST_DIRNAME}/tests_common.sh"
load "${BATS_TEST_DIRNAME}/confidential_kbs.sh"

setup() {
	setup_common || die "setup_common failed"
	auto_generate_policy_enabled || skip "confidential volume test requires an agent policy"
	is_runtime_rs || skip "confidential block devices are implemented by runtime-rs"
	is_coco_platform || skip "confidential block devices require a confidential guest"
	command -v kbs-client >/dev/null || skip "kbs-client is not installed"
	kubectl get service kbs -n coco-tenant >/dev/null 2>&1 || skip "Trustee/KBS is not deployed"

	case "${KATA_HYPERVISOR}" in
		*qemu*-snp-runtime-rs) ;;
		*) skip "the fixture currently requires an x86 SNP runtime-rs RuntimeClass" ;;
	esac

	test_id="${BATS_TEST_NUMBER}-$$"
	pod_name="confidential-rw-volume-${test_id}"
	storage_class="confidential-rw-${test_id}"
	pv_name="confidential-rw-pv-${test_id}"
	pvc_name="confidential-rw-pvc-${test_id}"
	resource_tag="confidential-rw-${test_id}"
	runtime_class="kata-${KATA_HYPERVISOR}"
	host_work_dir="/tmp/kata-confidential-rw-${test_id}"
	volume_image="${host_work_dir}/volume.img"
	key_file="${host_work_dir}/key.bin"
	mapper_name="kata-confidential-rw-${test_id}"
	loop_device=""
	luks_uuid="22222222-3333-4444-5555-666666666666"
	header_length_bytes=16777216

	infra_yaml="$(mktemp --tmpdir confidential-rw-infra.XXXXXX.yaml)"
	pod_yaml="$(mktemp --tmpdir confidential-rw-pod.XXXXXX.yaml)"
	policy_settings_dir="$(create_tmp_policy_settings_dir "${pod_config_dir}")"

	prepare_confidential_volume
	configure_kbs_resources
	write_test_manifests
	auto_generate_policy "${policy_settings_dir}" "${pod_yaml}"
	kubectl create -f "${infra_yaml}"
}

kbs_k8s_svc_host() {
	kubectl get svc "${KBS_SVC_NAME}" -n "${KBS_NS}" -o jsonpath='{.spec.clusterIP}'
}

kbs_k8s_svc_port() {
	kubectl get svc "${KBS_SVC_NAME}" -n "${KBS_NS}" -o jsonpath='{.spec.ports[0].port}'
}

exec_host_volume() {
	local node="$1"
	shift
	local command="$*"
	local pod_name
	pod_name="$(create_debugger_pod "${node}")"

	kubectl exec -qi -n kube-system "${pod_name}" -- \
		nsenter --mount=/host/proc/1/ns/mnt \
			--uts=/host/proc/1/ns/uts \
			--ipc=/host/proc/1/ns/ipc \
			--net=/host/proc/1/ns/net \
			-- bash -c "${command}" | tr -d '\r'
}

prepare_confidential_volume() {
	exec_host_volume "${node}" "sudo rm -rf '${host_work_dir}' && sudo install -d -m 0700 '${host_work_dir}/mnt'"
	exec_host_volume "${node}" sudo truncate -s 256M "${volume_image}"
	exec_host_volume "${node}" sudo openssl rand -out "${key_file}" 32
	exec_host_volume "${node}" sudo chmod 0600 "${key_file}"
	exec_host_volume "${node}" sudo cryptsetup --disable-locks luksFormat --batch-mode --type luks2 \
		--uuid "${luks_uuid}" --sector-size 4096 \
		--integrity hmac-sha256 \
		--key-file "${key_file}" "${volume_image}"
	exec_host_volume "${node}" sudo cryptsetup --disable-locks open --type luks2 --key-file "${key_file}" \
		"${volume_image}" "${mapper_name}"
	exec_host_volume "${node}" sudo mkfs.ext4 -F -E lazy_itable_init=0,lazy_journal_init=0 \
		-L confidential-model "/dev/mapper/${mapper_name}"
	exec_host_volume "${node}" sudo mount "/dev/mapper/${mapper_name}" "${host_work_dir}/mnt"
	exec_host_volume "${node}" "echo confidential-volume-e2e-ok | sudo tee '${host_work_dir}/mnt/model.txt' >/dev/null"
	exec_host_volume "${node}" sudo sync
	exec_host_volume "${node}" sudo umount "${host_work_dir}/mnt"
	exec_host_volume "${node}" sudo cryptsetup --disable-locks close "${mapper_name}"

	header_sha256="$(exec_host_volume "${node}" "sudo dd if='${volume_image}' bs=1 count='${header_length_bytes}' status=none | sha256sum | awk '{print \$1}'" | tr -d '\r\n')"
	[[ -n "${header_sha256}" ]]
	loop_device="$(exec_host_volume "${node}" sudo losetup --find --show "${volume_image}" | tr -d '\r\n')"
}

configure_kbs_resources() {
	local key_base64 manifest
	key_base64="$(exec_host_volume "${node}" sudo base64 -w0 "${key_file}" | tr -d '\r\n')"
	kbs_set_allow_all_resources
	kbs_set_resource_base64 "kata-ci" "storage-key" "${resource_tag}" "${key_base64}"
	manifest="$(jq -cn \
		--arg key_uri "kbs:///kata-ci/storage-key/${resource_tag}" \
		--arg luks_uuid "${luks_uuid}" \
		--arg header_sha256 "${header_sha256}" \
		--argjson header_length_bytes "${header_length_bytes}" \
		'{schemaVersion:1,volumeId:"model-data",volumeVersion:"v1",protection:{type:"luks2-integrity-rw",keyUri:$key_uri,luksUuid:$luks_uuid,header:{algorithm:"sha256",offsetBytes:0,lengthBytes:$header_length_bytes,sha256:$header_sha256}}}')"
	kbs_set_resource "kata-ci" "storage-manifest" "${resource_tag}" "${manifest}"
}

write_test_manifests() {
	sed \
		-e "s|STORAGE_CLASS|${storage_class}|g" \
		-e "s|PV_NAME|${pv_name}|g" \
		-e "s|PVC_NAME|${pvc_name}|g" \
		-e "s|LOOP_DEVICE|${loop_device}|g" \
		-e "s|NODE_NAME|${node}|g" \
		"${BATS_TEST_DIRNAME}/volume/confidential-ro-block-volume.yaml.in" >"${infra_yaml}"

	sed \
		-e "s|POD_NAME|${pod_name}|g" \
		-e "s|PVC_NAME|${pvc_name}|g" \
		-e "s|RUNTIME_CLASS|${runtime_class}|g" \
		-e "s|KBS_ADDRESS|$(kbs_k8s_svc_http_addr)|g" \
		-e "s|MANIFEST_URI|kbs:///kata-ci/storage-manifest/${resource_tag}|g" \
		"${BATS_TEST_DIRNAME}/runtimeclass_workloads/pod-confidential-rw-block-volume.yaml.in" >"${pod_yaml}"
}

@test "confidential read-write block volume is activated and writable" {
	kubectl create -f "${pod_yaml}"
	kubectl wait --for=jsonpath='{.status.phase}'=Succeeded --timeout=180s "pod/${pod_name}"
}

@test "confidential read-write block volume rejects corrupted LUKS header" {
	exec_host_volume "${node}" sudo losetup -d "${loop_device}"
	exec_host_volume "${node}" "sudo dd if=/dev/zero of='${volume_image}' bs=1 count=6 seek=0 conv=notrunc status=none"
	exec_host_volume "${node}" sudo losetup "${loop_device}" "${volume_image}"

	kubectl create -f "${pod_yaml}"
	cmd="kubectl get pod '${pod_name}' -o jsonpath='{.status.containerStatuses[0].state}' | grep -q StartError"
	waitForProcess 180 3 "${cmd}"
	run bash -c "kubectl get pod '${pod_name}' -o json | jq -r '.status.containerStatuses[0].state | .terminated.message // .waiting.message // \"\"'"
	[[ "${status}" -eq 0 ]]
	[[ "${output}" == *"authenticated LUKS header digest mismatch"* ]]
}

teardown() {
	kubectl describe pod "${pod_name:-}" 2>/dev/null || true
	kubectl delete pod "${pod_name:-}" --ignore-not-found --wait=true 2>/dev/null || true
	kubectl delete pvc "${pvc_name:-}" --ignore-not-found 2>/dev/null || true
	kubectl delete pv "${pv_name:-}" --ignore-not-found 2>/dev/null || true
	kubectl delete storageclass "${storage_class:-}" --ignore-not-found 2>/dev/null || true

	if [[ -n "${node:-}" && -n "${loop_device:-}" ]]; then
		exec_host_volume "${node}" sudo losetup -d "${loop_device}" 2>/dev/null || true
	fi
	if [[ -n "${node:-}" && -n "${host_work_dir:-}" ]]; then
		exec_host_volume "${node}" sudo umount "${host_work_dir}/mnt" 2>/dev/null || true
	fi
	if [[ -n "${node:-}" && -n "${mapper_name:-}" ]]; then
		exec_host_volume "${node}" "sudo cryptsetup --disable-locks close '${mapper_name}' || sudo dmsetup remove '${mapper_name}' || true" 2>/dev/null || true
	fi
	if [[ -n "${node:-}" && -n "${host_work_dir:-}" ]]; then
		exec_host_volume "${node}" sudo rm -rf "${host_work_dir}" 2>/dev/null || true
	fi

	rm -f "${infra_yaml:-}" "${pod_yaml:-}"
	[[ -z "${policy_settings_dir:-}" ]] || delete_tmp_policy_settings_dir "${policy_settings_dir}"
	[[ -z "${node:-}" ]] || teardown_common "${node}" "${node_start_time:-}"
}
