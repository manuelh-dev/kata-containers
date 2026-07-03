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
	pod_name="confidential-ro-volume-${test_id}"
	storage_class="confidential-ro-${test_id}"
	pv_name="confidential-ro-pv-${test_id}"
	pvc_name="confidential-ro-pvc-${test_id}"
	resource_tag="confidential-ro-${test_id}"
	runtime_class="kata-${KATA_HYPERVISOR}"
	host_work_dir="/tmp/kata-confidential-ro-${test_id}"
	volume_image="${host_work_dir}/volume.img"
	hash_image="${host_work_dir}/hash.img"
	key_file="${host_work_dir}/key.bin"
	mapper_name="kata-confidential-ro-${test_id}"
	loop_device=""
	luks_uuid="11111111-2222-3333-4444-555555555555"

	infra_yaml="$(mktemp --tmpdir confidential-ro-infra.XXXXXX.yaml)"
	pod_yaml="$(mktemp --tmpdir confidential-ro-pod.XXXXXX.yaml)"
	policy_settings_dir="$(create_tmp_policy_settings_dir "${pod_config_dir}")"

	prepare_confidential_volume
	configure_kbs_resources
	write_test_manifests
	auto_generate_policy "${policy_settings_dir}" "${pod_yaml}"
	kubectl create -f "${infra_yaml}"
}

prepare_confidential_volume() {
	exec_host "${node}" "sudo rm -rf '${host_work_dir}' && sudo install -d -m 0700 '${host_work_dir}/mnt'"
	exec_host "${node}" sudo truncate -s 128M "${volume_image}"
	exec_host "${node}" sudo openssl rand -out "${key_file}" 32
	exec_host "${node}" sudo chmod 0600 "${key_file}"
	exec_host "${node}" sudo cryptsetup luksFormat --batch-mode --type luks2 \
		--uuid "${luks_uuid}" --key-file "${key_file}" "${volume_image}"
	exec_host "${node}" sudo cryptsetup open --type luks2 --key-file "${key_file}" \
		"${volume_image}" "${mapper_name}"
	exec_host "${node}" sudo mkfs.ext4 -F -L confidential-model "/dev/mapper/${mapper_name}"
	exec_host "${node}" sudo mount "/dev/mapper/${mapper_name}" "${host_work_dir}/mnt"
	exec_host "${node}" "echo confidential-volume-e2e-ok | sudo tee '${host_work_dir}/mnt/model.txt' >/dev/null"
	exec_host "${node}" sudo chmod 0444 "${host_work_dir}/mnt/model.txt"
	exec_host "${node}" sudo sync
	exec_host "${node}" sudo umount "${host_work_dir}/mnt"
	exec_host "${node}" sudo cryptsetup close "${mapper_name}"

	exec_host "${node}" sudo truncate -s 2M "${hash_image}"
	verity_output="$(exec_host "${node}" sudo veritysetup format --no-superblock \
		--data-block-size 4096 --hash-block-size 4096 "${volume_image}" "${hash_image}")"
	root_hash="$(awk '/Root hash:/ {print $3}' <<<"${verity_output}" | tr -d '\r')"
	salt="$(awk '/Salt:/ {print $2}' <<<"${verity_output}" | tr -d '\r')"
	data_blocks="$(awk '/Data blocks:/ {print $3}' <<<"${verity_output}" | tr -d '\r')"
	[[ -n "${root_hash}" && -n "${salt}" && -n "${data_blocks}" ]]

	exec_host "${node}" sudo truncate -s 130M "${volume_image}"
	exec_host "${node}" "sudo dd if='${hash_image}' of='${volume_image}' bs=4096 seek='${data_blocks}' conv=notrunc status=none"
	loop_device="$(exec_host "${node}" sudo losetup --find --show --read-only "${volume_image}" | tr -d '\r\n')"
}

configure_kbs_resources() {
	local key_base64 manifest
	key_base64="$(exec_host "${node}" sudo base64 -w0 "${key_file}" | tr -d '\r\n')"
	kbs_set_allow_all_resources
	kbs_set_resource_base64 "kata-ci" "storage-key" "${resource_tag}" "${key_base64}"
	manifest="$(jq -cn \
		--arg key_uri "kbs:///kata-ci/storage-key/${resource_tag}" \
		--arg luks_uuid "${luks_uuid}" \
		--arg root_hash "${root_hash}" \
		--arg salt "${salt}" \
		--argjson data_blocks "${data_blocks}" \
		'{schemaVersion:1,volumeId:"model-data",volumeVersion:"v1",protection:{type:"luks2-verity-ro",keyUri:$key_uri,luksUuid:$luks_uuid,verity:{algorithm:"sha256",rootHash:$root_hash,salt:$salt,dataBlockSize:4096,hashBlockSize:4096,dataBlocks:$data_blocks}}}')"
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
		"${BATS_TEST_DIRNAME}/runtimeclass_workloads/pod-confidential-ro-block-volume.yaml.in" >"${pod_yaml}"
}

@test "confidential read-only block volume is activated and readable" {
	kubectl create -f "${pod_yaml}"
	kubectl wait --for=jsonpath='{.status.phase}'=Succeeded --timeout=180s "pod/${pod_name}"
	kubectl logs "${pod_name}" | grep -Fx confidential-volume-read-only-ok
}

@test "confidential read-only block volume rejects corrupted ciphertext" {
	exec_host "${node}" sudo losetup -d "${loop_device}"
	exec_host "${node}" "sudo dd if=/dev/zero of='${volume_image}' bs=1 count=6 seek=0 conv=notrunc status=none"
	exec_host "${node}" sudo losetup --read-only "${loop_device}" "${volume_image}"

	kubectl create -f "${pod_yaml}"
	cmd="kubectl get pod '${pod_name}' -o jsonpath='{.status.containerStatuses[0].state.terminated.reason}' | grep -q StartError"
	waitForProcess 180 3 "${cmd}"
	run kubectl get pod "${pod_name}" -o jsonpath='{.status.containerStatuses[0].state.terminated.message}'
	[[ "${status}" -eq 0 ]]
	[[ "${output}" == *"activate confidential block device"* ]]
}

teardown() {
	kubectl describe pod "${pod_name:-}" 2>/dev/null || true
	kubectl delete pod "${pod_name:-}" --ignore-not-found --wait=true 2>/dev/null || true
	kubectl delete pvc "${pvc_name:-}" --ignore-not-found 2>/dev/null || true
	kubectl delete pv "${pv_name:-}" --ignore-not-found 2>/dev/null || true
	kubectl delete storageclass "${storage_class:-}" --ignore-not-found 2>/dev/null || true

	if [[ -n "${node:-}" && -n "${loop_device:-}" ]]; then
		exec_host "${node}" sudo losetup -d "${loop_device}" 2>/dev/null || true
	fi
	if [[ -n "${node:-}" && -n "${host_work_dir:-}" ]]; then
		exec_host "${node}" sudo umount "${host_work_dir}/mnt" 2>/dev/null || true
	fi
	if [[ -n "${node:-}" && -n "${mapper_name:-}" ]]; then
		exec_host "${node}" sudo cryptsetup close "${mapper_name}" 2>/dev/null || true
	fi
	if [[ -n "${node:-}" && -n "${host_work_dir:-}" ]]; then
		exec_host "${node}" sudo rm -rf "${host_work_dir}" 2>/dev/null || true
	fi

	rm -f "${infra_yaml:-}" "${pod_yaml:-}"
	[[ -z "${policy_settings_dir:-}" ]] || delete_tmp_policy_settings_dir "${policy_settings_dir}"
	[[ -z "${node:-}" ]] || teardown_common "${node}" "${node_start_time:-}"
}
