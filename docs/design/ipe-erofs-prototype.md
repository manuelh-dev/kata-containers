# IPE and EROFS prototype

This opt-in prototype restricts executable code in the
`kata-qemu-nvidia-gpu-runtime-rs` guest to dm-verity-protected images whose root
hashes were known to the guest before the first workload container starts.

Build the NVIDIA artifacts with:

```sh
IPE_PROTOTYPE=yes make -f tools/packaging/kata-deploy/local-build/Makefile nvgpu-tarball
```

`IPE_PROTOTYPE=yes` makes the build:

- build the NVIDIA kernel with IPE and the experimental irreversible `seal`
  interface;
- build kata-agent with the prototype IPE code and device-mapper support;
- generate a dedicated throwaway IPE keypair, compile its certificate into the
  kernel trusted keyring, and copy the keypair into the measured NVIDIA base
  image; and
- add `agent.ipe_prototype` only to the generated
  `kata-qemu-nvidia-gpu-runtime-rs` configuration.

The agent records the root hash and algorithm of each EROFS layer only after its
dm-verity device and the final overlay mount have succeeded. The sandbox
container is allowed to start while setup is still in progress. At the first
non-sandbox `StartContainer`, the agent adds the rootfs and cold-plug extension
hashes from the kernel command line, generates and PKCS#7-signs an IPE policy,
activates it, enables enforcement, and writes `1` to
`/sys/kernel/security/ipe/seal`.

The policy allows non-execution IPE operations, denies execution by default,
and allows execution from only the collected dm-verity root hashes. Once
sealed, the kernel rejects changes through `new_policy`, `active`, `update`,
`delete`, and `enforce` for the rest of the VM lifetime.

## Prototype limitations

- The private key is deliberately present in the guest. The build refuses
  release or registry-push modes, but its artifacts must still be treated as
  test-only and must not be published.
- The first non-sandbox `StartContainer` is the finalization boundary. A later
  container whose image introduces another EROFS root hash cannot execute.
  Initially use this with single-container pods, or ensure all pod image layers
  are attached before the first workload starts. A production design needs an
  explicit pod-level finalize operation.
- The boot command line and EROFS metadata are trusted inputs in this prototype.
  A confidential-container version must authorize dynamic layer hashes or
  signatures through attestation and the agent security policy.
- The kernel change is experimental and local to the `ipe-experimental` build
  type; it is not an upstream IPE interface.
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
cat /sys/kernel/security/ipe/seal
cat /sys/kernel/security/ipe/policies/kata_verity/active
```

The active policy is visible at
`/sys/kernel/security/ipe/policies/kata_verity/policy`. An executable copied to
the writable overlay or `/run` should fail with `Permission denied`, while a
binary from an accepted dm-verity layer should execute.
