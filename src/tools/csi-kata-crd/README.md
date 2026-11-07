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
   information refers to the deterministic OCI device path injected by the
   webhook rather than to a driver-specific host device name.
5. Runtime-rs correlates that logical device path with the backing block
   device in the OCI specification, reuses its resolved host source for DAV,
   and asks the agent to mount the pre-existing filesystem at the original
   `/data` destination.

The shim awareness in step 5 is necessary because the Kata CSI service and the
backing CSI service receive independent requests. The webhook can communicate
stable identities and logical paths, but it runs before scheduling and before
the backing driver's `NodePublishVolume`; it cannot know the eventual
node-local host device path.

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
  next step will be conditional guest-side formatting: detect an existing
  filesystem, format only an empty device, and reject a conflicting format.
- The first implementation may leave the webhook-injected raw
  `volumeDevice` visible inside the application container while DAV mounts the
  same device at the requested directory. This is to reduce changes to the
  shim in the first iteration. If this works, the next iteration will have the
  shim consume and remove that OCI device and its cgroup permissions before
  creating the container.
