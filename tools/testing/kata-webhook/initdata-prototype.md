# Prototype: Injecting workload, cluster and OCI runtime information

## Motivation

This prototype extends the existing [webhook](README.md) implementation with
new capabilities for a redesign of the workload policing and attestation flow.

In a nutshell, these capabilities enable the webhook to inject the following
information/fragments via the init-data annotation:
- OCI and cluster settings
- pod spec and pod metadata
- agent security policy

This information can be picked up by a trusted guest component in order to
verify workloads and in order to forward this measurable and thus attestable
information to trustee.

## New webhook capabilities in a nutshell

- Users can start the webhook with a list of runtime classes for which the
  webhook will inject relevant fragments to the compressed init-data TOML of
  the `io.katacontainers.config.hypervisor.cc_init_data` annotation. (see the
  `INITDATA_RUNTIME_CLASSES` variable).
- The webhook can be parametrized to, e.g., preserve an existing agent security
  policy entry (`policy.rego`), or to overwrite it with a static policy, ...
- Pods using other runtime classes are left unchanged, even if they carry init-data.

New tooling and Makefile targets allow build and deployment of the webhook, as well
as extraction of information from the guest a guest-side enforcer/watchdog would be
using. This information is what is being injected, plus some information which
kata-agent would generate. A watchdog/enforcer can use this information to help the
agent make an educated decision on whether to allow or disallow the workloads the
shim wants to run via CreateContainer, ...

End-to-end use is documented below; it should be very easy for an agent to parse this
branch or documentation to get things going.

Prototyping and testing was done merely on a dev machine hosting a single-node Kubernetes cluster,
using GPU operator and/or the kata-device-plugin and -provisioner using NVIDIA GPU SNP runtime
classes.

## Deployment flows

The following are just a conglomeration of example commands to get there...
...you might need to adjust things.

Change to this directory:

```bash
cd tools/testing/kata-webhook
```

Build the webhook image and make it available to Kubernetes on your node:
```
make prototype-build
```

Note: this step imports the image into containerd's k8s.io namespace with
`sudo ctr images import` and the deployment uses `imagePullPolicy: Never`.
For different scenarios, you need to customize things.

Deploy the webhook:
```bash
make prototype-deploy-webhook
```

Note: the webhook registration uses `reinvocationPolicy: IfNeeded`. It does not solve
webhook finality, as we know. More important information on webhook deployment variables
is below!

If you want to deploy one of the samples provided in the samples directory,
export the `POD_SOURCE` environment variable:
```bash
export POD_SOURCE=samples/nvidia-gpu-snp.yaml
```

Note: you can also just deploy your own pods.

If you defined the `POD_SOURCE` variable, you can generate an agent security policy:
```bash
make prototype-prepare-tools
make prototype-generate-policy
```
The prepare tools step acquires genpolicy, the second step adds the agent security policy
to the respective pod's init-data section. For this, the make target copies the modified
sample into an `artifacts/` directory.

Deploy that pod manifest from the `artifacts/` directory:
```bash
make prototype-deploy-pod
```
Deploy the webhook with `AGENT_POLICY_MODE=set-if-absent` to give pods a static policy if
no policy was supplied in the manifest's annotation. Read more on pod deployment options
further below.

Note: Likely, you will just want to deploy a pod by hand without an agent security policy
as the gist is to pretty much have the webhook inject a static policy. The advantage of
adding a pod manifest to samples, then at least specifying the variable and running
`prototype-deploy-pod` is that we could version this file and its extracted artifacts,
for now as a database of samples the enforcer/watchdog must be able to handle.

To extract information from the guest after deployment, use:
```bash
make prototype-inspect
```
This information can be fed to a watchdog/enforcer outside a Kata pod.

Notes:
- `make prototype-all` runs a set sequence in one step, please consult the Makefile.
  This could be good for a quick end-to-end test.
- `make prototype-direct-all` runs the same complete flow but without using genpolicy.
- `make prototype-deploy-source` deploys a pod referenced via `POD_SOURCE` directly without
  genpolicy.
- run `make prototype-cleanup` removes the prototype pod and webhook.

## Important: what you target you actually want to run

Last but not least, the actual most important part - what you will actually want to run:
```bash
AGENT_POLICY_MODE=set-if-absent \
OCI_SETTINGS_FILE=samples/settings/oci-settings.json \
CLUSTER_SETTINGS_FILE=samples/settings/cluster-settings.json \
POD_SOURCE=samples/identity-and-volumes.yaml \
ARTIFACT_DIR="$PWD/artifacts/identity-and-volumes" make prototype-direct-all
```
Something like this let's you select a manifest, use the static policy, inject everything,
then extract everything.

## Explanations on pod manifest samples

`POD_SOURCE` defaults to
[`samples/nvidia-gpu-snp.yaml`](samples/nvidia-gpu-snp.yaml). It accepts a
path relative to this directory or an absolute path. The manifest must contain
one Pod named `cuda-vectoradd-kata-snp-policy-debug` in the `default` namespace,
using one of the `INITDATA_RUNTIME_CLASSES`. Both samples use
`kata-qemu-nvidia-gpu-snp-runtime-rs-devkit`. Additional manifests can be
collected under `samples/`. For NVIDIA runtime classes we need `-devkit` as only
with these, kata-ctl can grab guest-side artifacts (`kata-ctl exec ...`).

Two samples are included:

- `samples/nvidia-gpu-snp.yaml` runs a CUDA workload with a passthrough GPU.
- `samples/identity-and-volumes.yaml` creates its own ConfigMap and Secret. Its
  single container exercises `emptyDir`, ConfigMap, and Secret mounts together
  with explicit user, group, supplemental groups, and `fsGroup` settings.

## Variables for runtime class selection

`INITDATA_RUNTIME_CLASSES` is a comma-separated list and defaults to
`kata-qemu-nvidia-gpu-snp-runtime-rs-devkit`. Set it before
`make prototype-deploy-webhook`, for example to target TDX instead:

```bash
export INITDATA_RUNTIME_CLASSES=kata-qemu-nvidia-gpu-tdx-runtime-rs-devkit
```

## Variables (or, modes) for injecting the agent security policy

`AGENT_POLICY_MODE` selects how `make prototype-deploy-webhook` configures the
webhook to handle `policy.rego` in init-data:

| Mode | Existing `policy.rego` | No `policy.rego` |
| --- | --- | --- |
| `preserve` (default) | kept | none added |
| `set-if-absent` | kept | static policy added |
| `replace` | replaced by the static policy | static policy added |
| `fail-if-present` | Pod rejected | static policy added |

The static policy defaults to [`policy/static-policy.rego`](policy/static-policy.rego);
select another file with `AGENT_POLICY_FILE`:

```bash
AGENT_POLICY_MODE=set-if-absent make prototype-deploy-webhook
```

The static policy of this prototype does not depend on the pod manifest.
More details on the decisions it makes and why are in
[`policy/README.md`](policy/README.md). This file also lists each Agent ttRPC
request, and how that compares with genpolicy.

## Variables for injecting OCI and cluster settings

The webhook can also inject two JSON files that describe the environment the
Pod runs in, rather than the Pod itself:

- `oci-settings.json`, for OCI-version-dependent container defaults, such as
  `{"1.2.0":{"namespaces":["ipc","uts","mount"]}}`
- `cluster-settings.json`, for cluster facts such as service addresses, for
  example `{"services":[{"name":"kubernetes","clusterIP":"10.96.0.1"}]}`

The webhook treats both as opaque JSON; this prototype does not define their
schema. [`samples/settings/`](samples/settings/) contains the two examples
above as files. They are placeholders rather than a description of a real
cluster.
`make prototype-deploy-webhook` stores each file in a ConfigMap mounted into
the webhook:

| Entry | File variable | ConfigMap |
| --- | --- | --- |
| `oci-settings.json` | `OCI_SETTINGS_FILE` | `kata-initdata-webhook-oci-settings` |
| `cluster-settings.json` | `CLUSTER_SETTINGS_FILE` | `kata-initdata-webhook-cluster-settings` |

Unlike the Agent policy, the settings have no mode. Providing them is the
webhook's sole responsibility (at least for now), so when a file is given, its content always replaces
a possible entry. Without a file, the webhook leaves the entry alone.
The files must contain valid JSON:

```bash
OCI_SETTINGS_FILE=samples/settings/oci-settings.json \
CLUSTER_SETTINGS_FILE=samples/settings/cluster-settings.json \
make prototype-deploy-webhook
```

## Explanations on inspecting the guest

`make prototype-inspect` prints the captured data and exports, from the running
guest:

* the guest-pull image metadata database, `meta_store.json`
* the OCI `config.json` of each container, including the pause container
* the guest init-data, from which it extracts `pod-spec.json` and, if present,
  `pod-metadata.json`, `policy.rego`, `oci-settings.json`, and
  `cluster-settings.json`
* the CDI specifications that NVRC writes for GPUs, such as `nvidia.yaml`, if
  present

The guest files are byte-for-byte copies under
`artifacts/<pod-name>-<timestamp>/guest-root/`. The init-data entries and each
image configuration are also extracted into standalone files:

```text
artifacts/<pod-name>-<timestamp>/
├── guest-root/
│   ├── run/
│   │   ├── confidential-containers/initdata/initdata.toml
│   │   └── kata-containers/
│   │       ├── <container-id>/config.json
│   │       └── image/meta_store.json
│   └── var/run/cdi/
│       └── <CDI specification>
├── image-configs/
│   └── <image-digest>/
│       ├── image-config.json
│       ├── image-meta.json
│       └── reference.txt
├── cluster-settings.json
├── oci-settings.json
├── pod-metadata.json
├── pod-spec.json
└── policy.rego
```

Set `ARTIFACT_DIR` to choose a fixed output directory. Reusing that directory
replaces its previous capture, so stale container ID directories do not accumulate.
