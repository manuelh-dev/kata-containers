# Kata CRD CSI integration

This directory contains an experimental, generic integration for presenting an
existing CSI-provisioned block volume as a filesystem mount inside a Kata
Containers guest. It combines a custom resource, a controller, an admission
webhook, a small CSI driver, and runtime-rs awareness.

The neighboring [`csi-kata-directvolume`](../csi-kata-directvolume/) directory
contains a specific CSI driver and reference implementation of the Kata
[Direct Assigned Volume (DAV)](../../../docs/design/direct-blk-device-assignment.md)
interface. That driver provisions its own backing storage and registers it with
DAV. The integration described here does not provision the backing storage.
Instead, it layers a Kata-facing filesystem PVC over a block PVC supplied by an
existing CSI driver, such as Longhorn or NVMesh.

## API and workload

The prototype uses four user-visible resources:

1. A pre-provisioned `Block` PVC supplied by the original CSI driver.
2. A namespaced `KataVolume` custom resource referencing that backing PVC.
3. A Kata StorageClass defining the expected filesystem and mount defaults.
4. A Kata-facing `Filesystem` PVC whose `dataSourceRef` selects the
   `KataVolume`.

The StorageClass is shared across backing CSI implementations. A `KataVolume`
instance represents one backing-volume relationship; a separate CRD is not
required for each CSI driver.

The API names below are provisional:

```yaml
apiVersion: storage.k8s.io/v1
kind: StorageClass
metadata:
  name: kata-crd-ext4
provisioner: crdvolume.csi.katacontainers.io
parameters:
  csi.storage.k8s.io/fstype: ext4
mountOptions:
- noatime
---
apiVersion: storage.katacontainers.io/v1alpha1
kind: KataVolume
metadata:
  name: created-kv
spec:
  backingClaim: created-disk
---
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: created
spec:
  accessModes:
  - ReadWriteOnce
  volumeMode: Filesystem
  storageClassName: kata-crd-ext4
  dataSourceRef:
    apiGroup: storage.katacontainers.io
    kind: KataVolume
    name: created-kv
  resources:
    requests:
      storage: 10Gi
---
apiVersion: v1
kind: Pod
metadata:
  name: application
spec:
  runtimeClassName: kata
  containers:
  - name: application
    image: registry.k8s.io/e2e-test-images/busybox:1.36.1-1
    command: ["sh", "-c", "echo ready > /data/ready; sleep infinity"]
    volumeMounts:
    - name: data
      mountPath: /data
  volumes:
  - name: data
    persistentVolumeClaim:
      claimName: created
```

`created-disk`, which is not shown above, must initially be a statically
provisioned `Block` PVC containing a pre-existing ext4 filesystem. The
`fsType` setting describes the filesystem expected on the disk; the prototype
does not create it.

## Flow

Kubernetes does not transitively follow PVC or custom-resource references. The
backing PVC must therefore be materialized in the Pod:

```text
Pod volumeMount
    -> Kata-facing PVC
    -> KataVolume
    -> backing Block PVC
```

The components cooperate as follows:

1. The controller validates that the `KataVolume`, Kata-facing PVC, and backing
   PVC are in the same namespace. It verifies that the backing claim has
   `volumeMode: Block`, records its UID, and creates or reconciles the
   Kata-facing CSI PV.
2. During Pod admission, the webhook resolves the chain above. It adds the
   backing PVC to `spec.volumes` and injects a deterministic `volumeDevice`
   into every application container mounting the Kata-facing PVC.
3. The injected `volumeDevice` is required: kubelet ignores a volume that is
   listed in `spec.volumes` but is not referenced by a container. Referencing
   the Block PVC causes kubelet to invoke its original CSI driver and publish
   the device on the selected node.
4. The Kata CSI node service registers the outer mount through DAV. Its mount
   information uses the `oci-block` volume type and refers to the deterministic
   OCI device path injected by the webhook rather than to a driver-specific
   host device name.
5. Runtime-rs requires exactly one matching block-device entry in the OCI
   specification. It obtains its major and minor numbers, resolves the
   canonical host block-device path, and then uses existing DAV handling to
   hotplug it and ask the agent to mount the pre-existing filesystem at the
   original `/data` destination.

The shim awareness in step 5 is necessary because the Kata CSI service and the
backing CSI service receive independent requests. The webhook can communicate
stable identities and logical paths, but it runs before scheduling and before
the backing driver's `NodePublishVolume`; it cannot know the eventual
node-local host device path.

### Mutated Pod

The user authors only the Kata-facing `volumeMount`. Before the Pod is stored,
the webhook adds the backing PVC and a `volumeDevice` to each container that
mounts the Kata-facing PVC. Conceptually, the resulting Pod contains:

```yaml
spec:
  containers:
  - name: application
    volumeMounts:
    - name: data
      mountPath: /data
    volumeDevices:
    - name: kata-backing-1234567812341234
      devicePath: /dev/kata-crd-1234567812341234
  volumes:
  - name: data
    persistentVolumeClaim:
      claimName: created
  - name: kata-backing-1234567812341234
    persistentVolumeClaim:
      claimName: created-disk
```

The original CSI driver publishes `created-disk`. The Kata CSI driver does not
receive that driver's node-local device path; it knows only the deterministic
device path shared by the controller and webhook.

### DAV metadata

The Kata CSI node service writes DAV metadata for its own CSI target path:

```json
{
  "volume-type": "oci-block",
  "device": "/dev/kata-crd-1234567812341234",
  "fstype": "ext4",
  "options": [
    "noatime",
    "rw"
  ]
}
```

In the current prototype, `oci-block` changes `device` into a logical OCI
reference rather than a path expected to exist under the host's `/dev`. This
differs from `directvol`, where `device` is an actual host block-device or
backing-file path and can be passed directly to the hypervisor. This
overloading works, but the final DAV encoding is still under evaluation as
described below.

The Kata-facing CSI target looks similar to:

```text
/var/lib/kubelet/pods/.../volumes/kubernetes.io~csi/.../mount
```

That path identifies the outer DAV mount and ultimately provides the
container destination `/data`. It is a directory, not the backing block
device, and must not be hotplugged.

### Runtime resolution

After kubelet has asked the original CSI driver to publish the Block PVC, the
OCI specification conceptually contains both the outer mount and the injected
device:

```json
{
  "mounts": [
    {
      "source": "/var/lib/kubelet/pods/.../kata-facing-volume/mount",
      "destination": "/data",
      "type": "bind"
    }
  ],
  "linux": {
    "devices": [
      {
        "path": "/dev/kata-crd-1234567812341234",
        "type": "b",
        "major": 252,
        "minor": 7
      }
    ]
  }
}
```

Runtime-rs resolves the backing device as follows:

```text
/dev/kata-crd-1234567812341234
    -> exact OCI linux.devices match
    -> host device identity 252:7
    -> canonical host path, for example /dev/longhorn/volume-id
    -> QEMU hotplug
    -> agent ext4 mount
    -> bind mount at container destination /data
```

Reusing `directvol` for this metadata would make runtime-rs try to access
`/dev/kata-crd-1234567812341234` as a host path. It normally does not exist
there, and using host-path existence as a fallback signal would make the
contract ambiguous. The explicit `oci-block` type makes the required OCI
lookup fail closed.

The current minimal prototype leaves normal `volumeDevice` handling enabled.
The same guest block device is therefore both mounted at `/data` and exposed
raw inside the container at `/dev/kata-crd-1234567812341234`. Removing the raw
device and its cgroup permission is a later hardening step.

## Design variants under evaluation

Three parts of the design remain deliberately open: how DAV represents the
logical OCI device reference, which component suppresses the raw device from
the application container, and which component initializes a blank
filesystem.

### Device-reference encoding

The prototype must explicitly distinguish an actual host source from a logical
OCI device reference. Inferring the meaning from path existence or from a
reserved path prefix would be ambiguous and could select the wrong host
device. Two API shapes are still being evaluated.

#### Dedicated `oci-block` type

The first option keeps the new volume type but replaces the overloaded
`device` value with an explicit field:

```json
{
  "volume-type": "oci-block",
  "oci-device-path": "/dev/kata-crd-1234567812341234",
  "fstype": "ext4",
  "options": [
    "noatime",
    "rw"
  ]
}
```

The validation rules would be:

- `directvol` requires `device`, which remains an actual host path.
- `oci-block` requires `oci-device-path` and rejects `device`.
- `oci-device-path` must match exactly one OCI block-device entry.

This is the most explicit and fail-closed representation, but adds another DAV
volume type.

#### Extend `directvol` with a device-reference form

The second option retains `volume-type: directvol` and makes the source form
explicit within that type. One possibility is a dedicated top-level field:

```json
{
  "volume-type": "directvol",
  "oci-device": "/dev/kata-crd-1234567812341234",
  "fstype": "ext4",
  "options": [
    "noatime",
    "rw"
  ]
}
```

Another possibility keeps the existing `device` field and adds a metadata
discriminator:

```json
{
  "volume-type": "directvol",
  "device": "/dev/kata-crd-1234567812341234",
  "fstype": "ext4",
  "metadata": {
    "device-reference": "oci"
  },
  "options": [
    "noatime",
    "rw"
  ]
}
```

Retaining `directvol` avoids adding a new volume type and treats both forms as
different ways of identifying a directly assigned block volume. A dedicated
field is stronger than an untyped metadata key, however, and overloading
`device` remains less clear because that field has historically meant a host
path.

Whichever representation is selected must require exactly one source form and
must validate the logical reference against the OCI block-device list. This
encoding decision is independent of whether the shim or the agent ultimately
suppresses the raw device from the application container.

### Raw-device suppression: shim or agent

Removing the injected OCI device in runtime-rs is not the only possible
hardening design. The OCI device entry could remain long enough for normal
runtime device handling to attach the disk and determine its guest PCI or SCSI
identity. DAV handling would then reuse that already-attached identity rather
than independently request another attachment.

The intended flow would be:

```text
OCI linux.devices entry
    -> runtime-rs attaches the disk and obtains its guest identity

mountInfo.json
    -> associates that device with ext4, mount options, and /data

agent
    -> mounts the device through Storage
    -> does not expose the raw device to the application container
```

`mountInfo.json` would still be required. It carries the filesystem intent and
associates the Kata-facing mount with the logical OCI device. The difference is
that the OCI device entry, rather than DAV volume handling, would be the sole
owner of the QEMU attachment.

This design needs an explicit shim-to-agent signal that the device is an
internal storage carrier. Today the agent receives both a `Device`, meaning
"expose this disk at `/dev/kata-crd-...`", and a `Storage`, meaning "mount this
disk as a filesystem". It cannot safely infer that every device which also
appears as storage should be hidden, because a workload might intentionally
request both interfaces.

The agent-side variant therefore necessarily changes both agent behavior and
the shim-to-agent data contract. `CreateContainerRequest` must carry the
association or storage-only intent, either through a new protocol field or by
assigning new semantics to an existing field such as
`Storage.driver_options`. The latter may avoid a protobuf schema change, but
it is still an interface and policy change.

#### Variant: shim-side filtering

Runtime-rs attaches the OCI device, removes its matching `linux.devices` entry
from the OCI specification, and filters the generated Agent `Device` while
retaining the corresponding `Storage`. This requires a mutable OCI
specification but no agent or shim-to-agent interface changes.

#### Variant: agent-side filtering

Runtime-rs sends both objects with a new storage-only marker. The agent mounts
the `Storage` but omits the raw `Device` when creating the container. This
requires an agent protocol or semantic change and corresponding policy
support.

Agent-side enforcement is attractive for a future confidential-computing
design because it occurs inside the trusted guest. It is, however, a larger
cross-component change than shim-side filtering. The prototype does not yet
choose between these hardening approaches.

### Filesystem initialization: shim or agent

The current prototype requires the backing block device to contain an existing
ext4 filesystem. Supporting newly provisioned blank volumes requires an
explicit filesystem policy rather than treating every mount failure as
evidence that the device is empty. At minimum, the policy should distinguish:

```text
mount-only
format-if-empty
```

For `format-if-empty`, a matching ext4 filesystem is mounted unchanged, a
conclusively blank device is formatted, and a conflicting, damaged, or
ambiguous device is rejected without reformatting.

Kata already has a `create_filesystem` Agent `Storage.driver_options` value for
fresh block-backed `emptyDir` volumes. The agent responds by force-formatting
ext4. That behavior is useful precedent for transporting filesystem-creation
intent, but it is unsafe for persistent volumes because it assumes the device
is freshly allocated and disposable.

#### Variant: shim-side formatting

One possible implementation would have runtime-rs probe or format the
canonical host block-device path. This is technically possible after the shim
has resolved the OCI device, but it is not the preferred design:

- the host device may not be the final guest-visible layer once guest-side
  encryption or integrity is introduced;
- probing on the host and using the device later in the guest creates a
  time-of-check/time-of-use gap;
- an unsupported or encrypted filesystem must not be mistaken for a blank
  device;
- the host runtime would need filesystem tooling and formatting policy;
- for confidential workloads, the host is outside the trusted boundary.

#### Variant: agent-side formatting

The preferred responsibility split is:

```text
Kata StorageClass, CRD, and CSI service
    -> declare ext4 and format-if-empty intent in DAV metadata

runtime-rs
    -> translate that intent into Agent Storage driver options

agent
    -> probe the exact guest device immediately before use
    -> format only when conclusively blank
    -> mount the resulting filesystem
```

This approach requires agent implementation and policy support, and introduces
new shim-to-agent semantics even if an existing `driver_options` field is used
instead of adding a protobuf field. Shim-side formatting remains a useful
comparison variant, but the agent is the preferred executor.

## Planned first use

The first end-to-end test does not require an external backing CSI driver. It
uses the same static local-block pattern as the Kubernetes trusted-storage
tests: a host file is attached to a loop device and exposed through a
node-affine local PV.

Run the following commands on the selected Kubernetes node:

```bash
sudo truncate -s 1G /tmp/kata-crd-volume.img
loop_device=$(sudo losetup --find --show /tmp/kata-crd-volume.img)
sudo mkfs.ext4 -F "${loop_device}"
echo "${loop_device}"
```

Substitute the resulting loop-device path and the node name in this backing
storage manifest:

```yaml
apiVersion: storage.k8s.io/v1
kind: StorageClass
metadata:
  name: local-storage
provisioner: kubernetes.io/no-provisioner
volumeBindingMode: WaitForFirstConsumer
---
apiVersion: v1
kind: PersistentVolume
metadata:
  name: kata-crd-backing-pv
spec:
  capacity:
    storage: 1Gi
  volumeMode: Block
  accessModes:
  - ReadWriteOnce
  persistentVolumeReclaimPolicy: Retain
  storageClassName: local-storage
  local:
    path: /dev/loopN
  nodeAffinity:
    required:
      nodeSelectorTerms:
      - matchExpressions:
        - key: kubernetes.io/hostname
          operator: In
          values:
          - worker-node-name
---
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: created-disk
spec:
  accessModes:
  - ReadWriteOnce
  volumeMode: Block
  storageClassName: local-storage
  resources:
    requests:
      storage: 1Gi
```

After creating this backing PVC, apply the `KataVolume`, Kata-facing PVC, and
Pod shown above. The Pod should see the existing ext4 filesystem at `/data`.
Writing a marker, deleting the Pod, and creating it again must preserve the
marker.

This setup exercises the CRD controller, admission mutation, local block
publication, OCI device correlation, and DAV guest mount without introducing
backend-specific behavior. Longhorn and NVMesh Block PVCs are subsequent test
cases. Cleanup must delete the Pod, PVCs, and PV before detaching the loop
device and removing its backing file.

## Planned source layout

The intended directory structure is:

```text
csi-kata-crd/
|-- cmd/
|   |-- csi-kata-crd-controller/
|   |-- csi-kata-crd-node/
|   `-- csi-kata-crd-webhook/
|-- pkg/
|   |-- apis/storage/v1alpha1/
|   |-- contract/
|   |-- controller/
|   |-- csi/
|   `-- webhook/
|-- deploy/
|-- examples/
|-- Makefile
`-- README.md
```

- `cmd/csi-kata-crd-controller/` builds the CRD and PVC reconciliation
  service.
- `cmd/csi-kata-crd-node/` builds the CSI identity and node services. The node
  service registers and removes DAV metadata during `NodePublishVolume` and
  `NodeUnpublishVolume`.
- `cmd/csi-kata-crd-webhook/` builds the Pod admission webhook.
- `pkg/apis/storage/v1alpha1/` defines the `KataVolume` API and generated
  Kubernetes types.
- `pkg/contract/` holds shared names, deterministic device-path generation,
  and validation used by all three binaries.
- `pkg/controller/` contains CRD, PVC, and PV reconciliation.
- `pkg/csi/` contains the CSI implementation and DAV registration.
- `pkg/webhook/` resolves Pod volumes and performs idempotent mutation.
- `deploy/` contains the CRD, RBAC, webhook registration, CSI registration,
  controller Deployment, node DaemonSet, and Kata StorageClass.
- `examples/` contains a pre-provisioned backing-volume example and an
  end-to-end Kata workload.

## Planned build and deployment

The planned local build interface is:

```bash
make -C src/tools/csi-kata-crd
make -C src/tools/csi-kata-crd image \
    IMAGE=quay.io/example/csi-kata-crd:prototype
```

The runtime must also contain the runtime-rs device-to-DAV correlation change.
After making the image available to every Kubernetes node, the intended
deployment flow is:

```bash
kubectl apply -f src/tools/csi-kata-crd/deploy/crd.yaml
kubectl apply -f src/tools/csi-kata-crd/deploy/rbac.yaml
kubectl apply -f src/tools/csi-kata-crd/deploy/controller.yaml
kubectl apply -f src/tools/csi-kata-crd/deploy/webhook.yaml
kubectl apply -f src/tools/csi-kata-crd/deploy/csi-node.yaml
kubectl apply -f src/tools/csi-kata-crd/deploy/storageclass.yaml
kubectl apply -f src/tools/csi-kata-crd/examples/
```

The initial end-to-end test will verify that a Pod using only the Kata-facing
PVC can mount the existing ext4 filesystem, preserve data across Pod
recreation, and use the original backing CSI driver without modifying it.

## Next steps

- The first implementation assumes a statically provisioned backing volume
  with an existing ext4 filesystem. It does not format blank devices. The
  next step will be conditional guest-side formatting. A normal CSI
  Filesystem volume communicates this intent through
  `VolumeCapability.Mount`: the node plugin ensures that the requested
  filesystem exists, creating it only when the device is blank. The outer
  Kata CSI service receives the same Mount capability even though the
  original backing driver receives a Block capability. Runtime-rs should
  translate those standard Filesystem semantics into an Agent
  `format-if-empty` request. The agent should preserve a matching filesystem,
  format only a conclusively empty device, and reject conflicting, damaged,
  or ambiguous content. A separate `mount-only` override may still be useful
  for imported volumes where formatting must never occur.
- The first implementation may leave the webhook-injected raw
  `volumeDevice` visible inside the application container while DAV mounts the
  same device at the requested directory. This is to reduce changes to the
  shim in the first iteration. If this works, the next iteration will have the
  shim consume and remove that OCI device and its cgroup permissions before
  creating the container.
