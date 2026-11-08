// Copyright (c) 2026 Kata Containers contributors
//
// SPDX-License-Identifier: Apache-2.0

package v1alpha1

import (
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/runtime"
)

type KataVolumeSpec struct {
	BackingClaim string `json:"backingClaim"`
}

type KataVolumeStatus struct {
	BackingClaimUID string             `json:"backingClaimUID,omitempty"`
	DevicePath      string             `json:"devicePath,omitempty"`
	Conditions      []metav1.Condition `json:"conditions,omitempty"`
}

type KataVolume struct {
	metav1.TypeMeta   `json:",inline"`
	metav1.ObjectMeta `json:"metadata,omitempty"`

	Spec   KataVolumeSpec   `json:"spec,omitempty"`
	Status KataVolumeStatus `json:"status,omitempty"`
}

func (in *KataVolume) DeepCopyObject() runtime.Object {
	if in == nil {
		return nil
	}
	out := new(KataVolume)
	*out = *in
	out.ObjectMeta = *in.ObjectMeta.DeepCopy()
	if in.Status.Conditions != nil {
		out.Status.Conditions = append([]metav1.Condition(nil), in.Status.Conditions...)
	}
	return out
}

type KataVolumeList struct {
	metav1.TypeMeta `json:",inline"`
	metav1.ListMeta `json:"metadata,omitempty"`
	Items           []KataVolume `json:"items"`
}

func (in *KataVolumeList) DeepCopyObject() runtime.Object {
	if in == nil {
		return nil
	}
	out := new(KataVolumeList)
	*out = *in
	out.ListMeta = in.ListMeta
	if in.Items != nil {
		out.Items = make([]KataVolume, len(in.Items))
		for index := range in.Items {
			copied := in.Items[index].DeepCopyObject().(*KataVolume)
			out.Items[index] = *copied
		}
	}
	return out
}
