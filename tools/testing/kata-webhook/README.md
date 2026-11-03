# Kata Admission controller webhook

Implement a simple admission controller webhook to annotate pods with the
Kata runtime class.

## How to build the admission controller

> **Note:**
> Only run this step if you are modifying the current webhook or don't
> want to use the webhook available in docker hub.

First build the admission controller image and the associated
Kubernetes YAML files required to instantiate the admission
controller.

```bash
make IMAGE=quay.io/kata-containers/kata-webhook-example:latest
```

The Go toolchain used to build the image defaults to the version in
`versions.yaml` at the repository root, matching the `go` directive in
`go.mod`. Reading it requires [`yq`](https://github.com/mikefarah/yq) to be
installed. Override it with `GO_VERSION` if you need a different toolchain:

```bash
make GO_VERSION=1.26.7 IMAGE=quay.io/kata-containers/kata-webhook-example:latest
```

> **Note**
> Image needs to be published for the webhook needs to work. Alternately
> on a single machine cluster change the `imagePullPolicy` to use the locally
> built image.

## Making Kata the default runtime using an admission controller

Today in `crio.conf` `runc` is the default runtime when a user does not specify
`runtimeClass` in the pod spec. If you want to run a cluster where Kata is used
by default, except for workloads we know for sure will not work with Kata, use
the [admission webhook](https://kubernetes.io/docs/reference/access-authn-authz/extensible-admission-controllers/#admission-webhooks)
and sample admission controller we created by running the commands below:

> **Note**
>
> By default, the `runtimeClass` name used in this webhook is `kata`. If your
> cluster is configured with another `runtimeClass`, you'll need to change the
> value of the `RUNTIME_CLASS` environment variable defined in the
> [webhook file](deploy/webhook.yaml). You can manually edit the file or run:
>
> `export RUNTIME_CLASS=<>`
>
> `kubectl create cm kata-webhook --from-literal runtime_class=$RUNTIME_CLASS`

```bash
./create-certs.sh
kubectl apply -f deploy/
```

Afterwards you can run the `webhook-check.sh` script to check the webhook was
deployed correctly and is working:

```bash
./webhook-check.sh
```

The webhook mutates pods to use the Kata runtime class for all pods except
those with

* `hostNetwork: true`
* namespace: `rook-ceph` and `rook-ceph-system`

## Injecting a PodSpec into existing init-data

When a Pod already has the
`io.katacontainers.config.hypervisor.cc_init_data` annotation, the webhook also
adds the admission-time `PodSpec` JSON to `data["pod-spec.json"]` in the
compressed init-data TOML. Existing entries such as `policy.rego` are
preserved.

This is a prototype: scope its webhook registration to Pods that already have
the init-data annotation, and use `reinvocationPolicy: IfNeeded`. It does not
solve webhook finality if another mutating webhook changes the Pod after this
one.

The complete NVIDIA SNP prototype recipe can be run locally on a development
machine hosting a single-node Kubernetes cluster. The cluster must have a
working `kata-qemu-nvidia-gpu-snp-runtime-rs-devkit` RuntimeClass, the NVIDIA
GPU Operator configured for confidential computing, and an allocatable
`nvidia.com/pgpu` device.

Run each stage in sequence:

```bash
make prototype-prepare-tools
make prototype-build
make prototype-deploy-webhook
make prototype-generate-policy
make prototype-deploy-pod
make prototype-inspect
```

Alternatively, run the complete sequence:

```bash
make prototype-all
```

### Pod manifest samples

The default input is
[`samples/nvidia-gpu-snp.yaml`](samples/nvidia-gpu-snp.yaml). The recipe copies
the input into the normally ignored `artifacts/` directory before genpolicy
adds the init-data annotation, so the source manifest remains unchanged.

Select another sample or an external Pod manifest with `POD_SOURCE`:

```bash
export POD_SOURCE=/path/to/pod.yaml
make prototype-all
```

The input must contain one Pod using the prototype's expected name, `default`
namespace, and `kata-qemu-nvidia-gpu-snp-runtime-rs-devkit` RuntimeClass.
Additional manifests can be collected under `samples/`.

The self-contained `samples/identity-and-volumes.yaml` example creates its own
ConfigMap and Secret. Its single container exercises `emptyDir`, ConfigMap, and
Secret mounts together with explicit user, group, supplemental groups, and
`fsGroup` settings:

```bash
POD_SOURCE=samples/identity-and-volumes.yaml make prototype-all
```

A captured run of this sample is checked in under
`artifacts/identity-and-volumes/`.

The existing `make all` target remains build-only and does not modify a
cluster. Use `make prototype-cleanup` to remove the prototype Pod and webhook.
The underlying script also accepts the same stages as subcommands; run it
without arguments to list them.

The inspection stage prints the captured data and leaves byte-for-byte copies
of the guest files under `artifacts/<pod-name>-<timestamp>/guest-root/`. It
also extracts the embedded PodSpec and each image configuration into convenient
standalone JSON files:

```text
artifacts/<pod-name>-<timestamp>/
├── SHA256SUMS
├── guest-root/
│   └── run/
│       ├── confidential-containers/initdata/initdata.toml
│       └── kata-containers/
│           ├── <container-id>/config.json
│           └── image/meta_store.json
├── image-configs/
│   └── <image-digest>/
│       ├── image-config.json
│       ├── image-meta.json
│       └── reference.txt
└── pod-spec.json
```

`meta_store.json` is the durable guest-pull image metadata database. Its
`image_db[*].image_config` entries contain the OCI image configurations used to
create the container bundles. The per-image files under `image-configs/` are
derived from that exact captured database. Set `ARTIFACT_DIR` to choose a
fixed output directory.

## Enforcing a minimum memory limit

Kata pods run inside a VM, so a memory limit that is comfortable under `runc`
can be too small once the guest kernel and agent are accounted for. The webhook
can raise any explicit memory limit that falls below a configured floor.

No minimum is enforced by default. To enable it, set the `min_memory_limit` key
of the `kata-webhook` ConfigMap in the [webhook file](deploy/webhook.yaml) to a
[quantity](https://kubernetes.io/docs/reference/kubernetes-api/common-definitions/quantity/).

Once configured, the webhook:

* raises `limits.memory` on every container and init container whose limit is
  below the minimum
* leaves containers without an explicit `limits.memory` untouched
* only applies to pods it actually moves to the Kata runtime class. Pods skipped
  for any of the reasons listed above, pods in the `sonobuoy` namespace, and pods
  that already set `runtimeClassName` keep their original limits
