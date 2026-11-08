// Copyright (c) 2026 Kata Containers contributors
//
// SPDX-License-Identifier: Apache-2.0

package webhook

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"

	"kata-containers/csi-kata-crd/pkg/contract"

	admissionv1 "k8s.io/api/admission/v1"
	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"k8s.io/client-go/dynamic"
	"k8s.io/client-go/kubernetes"
)

type Handler struct {
	kube    kubernetes.Interface
	dynamic dynamic.Interface
}

type patchOperation struct {
	Operation string      `json:"op"`
	Path      string      `json:"path"`
	Value     interface{} `json:"value,omitempty"`
}

func New(kube kubernetes.Interface, dynamicClient dynamic.Interface) *Handler {
	return &Handler{kube: kube, dynamic: dynamicClient}
}

func (handler *Handler) ServeHTTP(writer http.ResponseWriter, request *http.Request) {
	writer.Header().Set("Content-Type", "application/json")

	var review admissionv1.AdmissionReview
	if err := json.NewDecoder(request.Body).Decode(&review); err != nil {
		http.Error(writer, fmt.Sprintf("decode AdmissionReview: %v", err), http.StatusBadRequest)
		return
	}
	if review.Request == nil {
		http.Error(writer, "AdmissionReview has no request", http.StatusBadRequest)
		return
	}

	response := &admissionv1.AdmissionResponse{
		UID:     review.Request.UID,
		Allowed: true,
	}
	if review.Request.Resource.Group == "" &&
		review.Request.Resource.Resource == "pods" &&
		review.Request.Operation == admissionv1.Create {
		patch, err := handler.mutatePod(
			request.Context(),
			review.Request.Namespace,
			review.Request.Object.Raw,
		)
		if err != nil {
			response.Allowed = false
			response.Result = &metav1.Status{Message: err.Error()}
		} else if len(patch) != 0 {
			patchType := admissionv1.PatchTypeJSONPatch
			response.PatchType = &patchType
			response.Patch = patch
		}
	}

	review.Response = response
	review.Request = nil
	if err := json.NewEncoder(writer).Encode(review); err != nil {
		http.Error(writer, fmt.Sprintf("encode AdmissionReview: %v", err), http.StatusInternalServerError)
	}
}

func (handler *Handler) mutatePod(
	ctx context.Context,
	namespace string,
	raw []byte,
) ([]byte, error) {
	var pod corev1.Pod
	if err := json.Unmarshal(raw, &pod); err != nil {
		return nil, fmt.Errorf("decode Pod: %w", err)
	}
	if pod.Namespace != "" {
		namespace = pod.Namespace
	}

	var patches []patchOperation
	for volumeIndex := range pod.Spec.Volumes {
		volume := &pod.Spec.Volumes[volumeIndex]
		if volume.PersistentVolumeClaim == nil {
			continue
		}
		claim, err := handler.kube.CoreV1().
			PersistentVolumeClaims(namespace).
			Get(ctx, volume.PersistentVolumeClaim.ClaimName, metav1.GetOptions{})
		if err != nil {
			return nil, fmt.Errorf("get PVC %q: %w", volume.PersistentVolumeClaim.ClaimName, err)
		}
		if !isKataVolumeClaim(claim) {
			continue
		}

		source := claim.Spec.DataSourceRef
		if source.Namespace != nil && *source.Namespace != namespace {
			return nil, fmt.Errorf("cross-namespace KataVolume references are unsupported")
		}
		kataVolume, err := handler.dynamic.Resource(contract.KataVolumeGVR).
			Namespace(namespace).
			Get(ctx, source.Name, metav1.GetOptions{})
		if err != nil {
			return nil, fmt.Errorf("get KataVolume %q: %w", source.Name, err)
		}
		backingClaimName, found, err := unstructured.NestedString(
			kataVolume.Object,
			"spec",
			"backingClaim",
		)
		if err != nil || !found || backingClaimName == "" {
			return nil, fmt.Errorf("KataVolume %q has no spec.backingClaim", source.Name)
		}
		backingClaim, err := handler.kube.CoreV1().
			PersistentVolumeClaims(namespace).
			Get(ctx, backingClaimName, metav1.GetOptions{})
		if err != nil {
			return nil, fmt.Errorf("get backing PVC %q: %w", backingClaimName, err)
		}
		if backingClaim.Spec.VolumeMode == nil ||
			*backingClaim.Spec.VolumeMode != corev1.PersistentVolumeBlock {
			return nil, fmt.Errorf("backing PVC %q must have volumeMode Block", backingClaimName)
		}

		backingVolumeName, err := contract.BackingVolumeName(kataVolume.GetUID())
		if err != nil {
			return nil, err
		}
		devicePath, err := contract.DevicePath(kataVolume.GetUID())
		if err != nil {
			return nil, err
		}

		if err := addBackingVolume(&pod, backingVolumeName, backingClaimName, &patches); err != nil {
			return nil, err
		}
		if err := addVolumeDevices(
			&pod,
			volume.Name,
			backingVolumeName,
			devicePath,
			&patches,
		); err != nil {
			return nil, err
		}
	}

	if len(patches) == 0 {
		return nil, nil
	}
	patch, err := json.Marshal(patches)
	if err != nil {
		return nil, fmt.Errorf("encode JSON patch: %w", err)
	}
	return patch, nil
}

func isKataVolumeClaim(claim *corev1.PersistentVolumeClaim) bool {
	source := claim.Spec.DataSourceRef
	return source != nil &&
		source.APIGroup != nil &&
		*source.APIGroup == contract.APIGroup &&
		source.Kind == contract.KataVolumeKind
}

func addBackingVolume(
	pod *corev1.Pod,
	volumeName string,
	claimName string,
	patches *[]patchOperation,
) error {
	for index := range pod.Spec.Volumes {
		existing := &pod.Spec.Volumes[index]
		if existing.Name != volumeName {
			continue
		}
		if existing.PersistentVolumeClaim == nil ||
			existing.PersistentVolumeClaim.ClaimName != claimName {
			return fmt.Errorf("Pod volume %q conflicts with injected backing volume", volumeName)
		}
		return nil
	}

	volume := corev1.Volume{
		Name: volumeName,
		VolumeSource: corev1.VolumeSource{
			PersistentVolumeClaim: &corev1.PersistentVolumeClaimVolumeSource{
				ClaimName: claimName,
			},
		},
	}
	*patches = append(*patches, patchOperation{
		Operation: "add",
		Path:      "/spec/volumes/-",
		Value:     volume,
	})
	pod.Spec.Volumes = append(pod.Spec.Volumes, volume)
	return nil
}

func addVolumeDevices(
	pod *corev1.Pod,
	mountedVolumeName string,
	backingVolumeName string,
	devicePath string,
	patches *[]patchOperation,
) error {
	for containerIndex := range pod.Spec.Containers {
		container := &pod.Spec.Containers[containerIndex]
		usesVolume := false
		for _, mount := range container.VolumeMounts {
			if mount.Name == mountedVolumeName {
				usesVolume = true
				break
			}
		}
		if !usesVolume {
			continue
		}

		alreadyPresent := false
		for _, device := range container.VolumeDevices {
			if device.Name == backingVolumeName && device.DevicePath == devicePath {
				alreadyPresent = true
				break
			}
			if device.Name == backingVolumeName || device.DevicePath == devicePath {
				return fmt.Errorf(
					"container %q has a conflicting volumeDevice for %q",
					container.Name,
					backingVolumeName,
				)
			}
		}
		if alreadyPresent {
			continue
		}

		device := corev1.VolumeDevice{Name: backingVolumeName, DevicePath: devicePath}
		path := fmt.Sprintf("/spec/containers/%d/volumeDevices", containerIndex)
		value := interface{}([]corev1.VolumeDevice{device})
		if len(container.VolumeDevices) != 0 {
			path += "/-"
			value = device
		}
		*patches = append(*patches, patchOperation{
			Operation: "add",
			Path:      path,
			Value:     value,
		})
		container.VolumeDevices = append(container.VolumeDevices, device)
	}
	return nil
}

var _ http.Handler = (*Handler)(nil)
