# IPE and EROFS prototype

This opt-in prototype restricts executable code in the
`kata-qemu-nvidia-gpu-runtime-rs` guest to known dm-verity-protected boot disks
and dynamically signed dm-verity EROFS image layers.

Build the NVIDIA artifacts with:

```sh
IPE_PROTOTYPE=yes make -f tools/packaging/kata-deploy/local-build/Makefile nvgpu-tarball
```

`IPE_PROTOTYPE=yes` makes the build:

- build the NVIDIA kernel with IPE policy-controlled control-plane lockdown,
  a dedicated `.ipe` policy-signing keyring, and a backport of Linux 7.0's
  dedicated `.dm-verity` keyring;
- build kata-agent with the prototype IPE code and device-mapper support;
- add `agent.ipe_prototype`, `ipe.keyring_unsealed=1`, and
  `dm_verity.keyring_unsealed=1` only to the generated
  `kata-qemu-nvidia-gpu-runtime-rs` configuration.

When the first EROFS layer is attached, the agent generates an ephemeral RSA
layer-signing keypair in memory, installs its public certificate in
`.dm-verity`, and irreversibly seals that keyring. For each layer, it creates a
detached PKCS#7 signature over the root hash, publishes the signature as a
temporary thread-keyring user key in the same blocking thread that loads the
dm-verity table, and supplies `root_hash_sig_key_desc`. Keeping publication and
the ioctl in one thread is required because the kernel resolves the description
with `request_key()`. The temporary signature key is removed after the kernel
has copied and verified it.

The sandbox container may start while trusted guest setup continues. At the
first non-sandbox `StartContainer`, the agent destroys the ephemeral layer
private key. It also ensures `.dm-verity` is provisioned and sealed when no
EROFS layer was attached. The agent then creates a second, separate ephemeral
IPE policy-signing keypair, installs its public certificate in `.ipe`, and
irreversibly restricts that keyring. It creates an IPE policy containing
explicit root hashes for the rootfs and cold-plug extensions from the kernel
command line, plus a generic `dmverity_signature=TRUE` execution rule for
EROFS layers. The policy adds `op=IPE_CONFIG action=DENY`. The agent PKCS#7
signs the policy, drops the private key, enables enforcement, and activates
the policy.

The policy allows non-execution IPE operations, denies execution by default,
and allows execution only from explicitly listed boot disks or a dm-verity
device whose root-hash signature the kernel validated. Once activated, its
`IPE_CONFIG` rule rejects changes through `new_policy`, `active`, `update`,
`delete`, `enforce`, and `success_audit` for the rest of the VM lifetime.

## Prototype limitations

- The IPE policy and EROFS layer private keys are generated only in agent
  memory and dropped at finalization. The build still refuses release or
  registry-push modes because this is an experimental kernel and agent
  integration, not a production trust design.
- The first non-sandbox `StartContainer` is the finalization boundary. A later
  container whose image introduces another EROFS root hash cannot attach that
  layer.
  Initially use this with single-container pods, or ensure all pod image layers
  are attached before the first workload starts. A production design needs an
  explicit pod-level finalize operation.
- The boot command line and EROFS metadata are trusted inputs in this prototype.
  A confidential-container version must authorize dynamic layer hashes or
  signatures through attestation and the agent security policy.
- The `.dm-verity` keyring is an upstream Linux 7.0 interface backported to the
  Kata 6.18 kernel. The `.ipe` keyring and `IPE_CONFIG` operation remain
  experimental, local interfaces.
- This remains a hybrid prototype: it generates and signs the IPE policy in the
  guest instead of embedding a static policy in the kernel.
- The NVIDIA rootfs and GPU-extension builders UPX-compress most executable ELF
  files. UPX starts from the accepted dm-verity image but expands the real
  executable into an anonymous `memfd`; IPE then correctly denies its
  executable mapping because tmpfs has no accepted dm-verity identity. The
  already-running agent survives late activation, but any UPX-packed guest or
  container executable launched after activation can fail. An IPE-compatible
  production artifact must disable UPX, or comprehensively exclude every
  executable that can run after enforcement. Allowing executable tmpfs to
  accommodate UPX would defeat the intended policy.

Inside a protected guest, these should all print `1`:

```sh
cat /sys/kernel/security/ipe/enforce
cat /sys/kernel/security/ipe/policies/kata_verity/active
```

The active policy is visible at
`/sys/kernel/security/ipe/policies/kata_verity/policy`. An executable copied to
the writable overlay or `/run` should fail with `Permission denied`, while a
binary from an accepted dm-verity layer should execute.
