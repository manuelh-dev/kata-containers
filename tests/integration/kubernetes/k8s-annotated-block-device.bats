#!/usr/bin/env bats
#
# Copyright (c) 2026 NVIDIA Corporation
#
# SPDX-License-Identifier: Apache-2.0

# shellcheck disable=SC2154

load "${BATS_TEST_DIRNAME}/lib.sh"
load "${BATS_TEST_DIRNAME}/../../common.bash"
load "${BATS_TEST_DIRNAME}/confidential_common.sh"
load "${BATS_TEST_DIRNAME}/tests_common.sh"

setup() {
	is_runtime_rs || skip "Annotated block-device mounts require runtime-rs"
	! is_confidential_runtime_class || skip "CoCo block-device mounts are not supported yet"
	is_shared_fs_none_runtime_class || skip "Test requires a shared_fs=none runtime class"

	setup_common || die "setup_common failed"

	node="$(get_one_kata_node)"
	pod_name="annotated-block-device"
	loop_file="/tmp/annotated-block-device.img"
	mountpoint="/data"

	local_device="$(create_loop_device "${loop_file}" 128)"
	exec_host "${node}" "mkfs.ext4 -q -F '${local_device}'"

	storage_yaml="$(mktemp "${BATS_FILE_TMPDIR}/annotated-block-storage.XXXXXX.yaml")"
	LOCAL_DEVICE="${local_device}" NODE_NAME="${node}" envsubst \
		< "${pod_config_dir}/annotated-block-storage.yaml.in" > "${storage_yaml}"

	pod_yaml="$(mktemp "${BATS_FILE_TMPDIR}/pod-annotated-block-device.XXXXXX.yaml")"
	RUNTIMECLASS="$(get_test_runtime_class)" NODE_NAME="${node}" envsubst \
		< "${pod_config_dir}/pod-annotated-block-device.yaml.in" > "${pod_yaml}"

	policy_settings_dir="$(create_tmp_policy_settings_dir "${pod_config_dir}")"
	allow_requests "${policy_settings_dir}" "ExecProcessRequest" "ReadStreamRequest"
	auto_generate_policy "${policy_settings_dir}" "${pod_yaml}"
}

@test "Pod annotation mounts a volumeDevice filesystem in the guest" {
	retry_kubectl_apply "${storage_yaml}"
	kubectl create -f "${pod_yaml}"
	kubectl wait --for=condition=Ready --timeout="${timeout}" "pod/${pod_name}"

	mount_line="$(pod_exec "${pod_name}" sh -c "mount | grep ' on ${mountpoint} type ext4 '")"
	info "annotated block-device mount: ${mount_line}"
	[[ "${mount_line}" == /dev/* ]]

	[[ "$(pod_exec "${pod_name}" sh -c "echo annotation-volume-mount-ok > '${mountpoint}/result' && cat '${mountpoint}/result'")" == "annotation-volume-mount-ok" ]]

	run pod_exec "${pod_name}" test -b /dev/kata-data
	[[ "${status}" -ne 0 ]]
}

teardown() {
	if [[ "${BATS_TEST_COMPLETED:-}" != "1" && -n "${pod_name:-}" ]]; then
		kubectl describe "pod/${pod_name}" || true
	fi

	[[ -z "${pod_name:-}" ]] || kubectl delete pod "${pod_name}" --ignore-not-found --wait=true
	kubectl delete pvc annotated-block-pvc --ignore-not-found
	kubectl delete pv annotated-block-pv --ignore-not-found
	kubectl delete storageclass annotated-block-local --ignore-not-found

	[[ -z "${loop_file:-}" ]] || cleanup_loop_device "${loop_file}" || true
	[[ -z "${storage_yaml:-}" ]] || rm -f "${storage_yaml}"
	[[ -z "${pod_yaml:-}" ]] || rm -f "${pod_yaml}"
	delete_tmp_policy_settings_dir "${policy_settings_dir:-}"

	[[ -z "${node:-}" ]] || teardown_common "${node}" "${node_start_time:-}"
}
