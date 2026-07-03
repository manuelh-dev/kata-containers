# Confidential read-only block volumes

This experimental runtime-rs feature exposes a pre-populated block volume to a
confidential workload only after the guest has authenticated and decrypted it.
The host attaches an opaque read-only block device; it does not receive the
volume key or plaintext.

Enable the feature in the runtime configuration:

```toml
[runtime]
experimental = ["confidential_block_device"]
```

The pod supplies one confidential-volume annotation. Its JSON object maps each
Kubernetes volume name to a manifest URI and container device path;
`devicePath` must match that volume's `volumeDevices` entry exactly. The fixed,
slash-free annotation key is passed by containerd's standard
`io.katacontainers.*` annotation allowlist.

```yaml
metadata:
  annotations:
    io.katacontainers.volume.confidential-ro: >-
      {"block-disk":{"manifestUri":"kbs:///kata-ci/storage-manifest/model-data-v1","devicePath":"/dev/hostdisk"}}
spec:
  containers:
    - name: workload
      volumeDevices:
        - name: block-disk
          devicePath: /dev/hostdisk
  volumes:
    - name: block-disk
      persistentVolumeClaim:
        claimName: block-disk-pvc
        readOnly: true
```

The runtime rejects a matching device unless its OCI/host attachment is
read-only. It passes the guest transport address and manifest URI to the agent,
which resolves the physical block device and asks the Confidential Data Hub
(CDH) to activate it. CDH fetches the manifest and key from Trustee/KBS,
constructs a read-only dm-verity mapping over the ciphertext, verifies the LUKS
UUID, and opens the verified LUKS2 device read-only. Only the resulting mapper
device is inserted into the container, with read-only device-cgroup access.
Container teardown asks CDH to close both mappings.

The KBS manifest has this schema:

```json
{
  "schemaVersion": 1,
  "volumeId": "model-data",
  "volumeVersion": "v1",
  "protection": {
    "type": "luks2-verity-ro",
    "keyUri": "kbs:///kata-ci/storage-key/model-data-v1",
    "luksUuid": "00000000-0000-0000-0000-000000000000",
    "verity": {
      "algorithm": "sha256",
      "rootHash": "...",
      "salt": "...",
      "dataBlockSize": 4096,
      "hashBlockSize": 4096,
      "dataBlocks": 128000
    }
  }
}
```

The image layout is fixed: `dataBlocks * dataBlockSize` bytes of complete LUKS2
ciphertext begin at byte zero, followed immediately by a dm-verity hash tree
created with `veritysetup --no-superblock`. CDH derives the hash offset and
minimum device size; they are not duplicated in the manifest.

The exact annotation value and generated secure-device request are included in
the measured agent policy. Attestation therefore binds the workload to the
manifest reference for this scenario. The host still controls availability and
may withhold or corrupt the device, but ciphertext corruption causes activation
or subsequent reads to fail rather than returning unauthenticated plaintext.

The new CDH `SecureVolumeService` is intentionally separate from the existing
`SecureMountService`. Its activation/deactivation model can later support other
single-pod read-only or read-write secure volume profiles without changing this
initial profile or the existing encrypted `emptyDir` behavior.
