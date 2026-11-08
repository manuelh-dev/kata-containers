// Copyright (c) 2026 Kata Containers contributors
//
// SPDX-License-Identifier: Apache-2.0

package csi

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"net"
	"net/url"
	"os"
	"path/filepath"

	"kata-containers/csi-kata-crd/pkg/contract"

	csipb "github.com/container-storage-interface/spec/lib/go/csi"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

const mountInfoFileName = "mountInfo.json"

type Server struct {
	csipb.UnimplementedIdentityServer
	csipb.UnimplementedNodeServer

	nodeID           string
	directVolumeRoot string
}

type mountInfo struct {
	VolumeType string            `json:"volume-type"`
	Device     string            `json:"device"`
	FSType     string            `json:"fstype"`
	Metadata   map[string]string `json:"metadata,omitempty"`
	Options    []string          `json:"options,omitempty"`
}

func New(nodeID string, directVolumeRoot string) *Server {
	return &Server{
		nodeID:           nodeID,
		directVolumeRoot: directVolumeRoot,
	}
}

func (server *Server) Run(endpoint string) error {
	socketPath, err := unixSocketPath(endpoint)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(socketPath), 0750); err != nil {
		return fmt.Errorf("create CSI socket directory: %w", err)
	}
	if err := os.Remove(socketPath); err != nil && !os.IsNotExist(err) {
		return fmt.Errorf("remove stale CSI socket: %w", err)
	}

	listener, err := net.Listen("unix", socketPath)
	if err != nil {
		return fmt.Errorf("listen on CSI socket: %w", err)
	}
	grpcServer := grpc.NewServer()
	csipb.RegisterIdentityServer(grpcServer, server)
	csipb.RegisterNodeServer(grpcServer, server)
	return grpcServer.Serve(listener)
}

func unixSocketPath(endpoint string) (string, error) {
	parsed, err := url.Parse(endpoint)
	if err != nil {
		return "", fmt.Errorf("parse CSI endpoint: %w", err)
	}
	if parsed.Scheme != "unix" || parsed.Path == "" {
		return "", fmt.Errorf("CSI endpoint %q must be an absolute unix URL", endpoint)
	}
	return parsed.Path, nil
}

func (server *Server) GetPluginInfo(
	context.Context,
	*csipb.GetPluginInfoRequest,
) (*csipb.GetPluginInfoResponse, error) {
	return &csipb.GetPluginInfoResponse{
		Name:          contract.DriverName,
		VendorVersion: "prototype",
	}, nil
}

func (server *Server) GetPluginCapabilities(
	context.Context,
	*csipb.GetPluginCapabilitiesRequest,
) (*csipb.GetPluginCapabilitiesResponse, error) {
	return &csipb.GetPluginCapabilitiesResponse{}, nil
}

func (server *Server) Probe(
	context.Context,
	*csipb.ProbeRequest,
) (*csipb.ProbeResponse, error) {
	return &csipb.ProbeResponse{}, nil
}

func (server *Server) NodePublishVolume(
	_ context.Context,
	request *csipb.NodePublishVolumeRequest,
) (*csipb.NodePublishVolumeResponse, error) {
	if request.GetVolumeId() == "" {
		return nil, status.Error(codes.InvalidArgument, "volume ID is missing")
	}
	if request.GetTargetPath() == "" {
		return nil, status.Error(codes.InvalidArgument, "target path is missing")
	}
	mountCapability := request.GetVolumeCapability().GetMount()
	if mountCapability == nil {
		return nil, status.Error(codes.InvalidArgument, "mount volume capability is required")
	}
	devicePath := request.GetVolumeContext()[contract.AttributeDevicePath]
	if devicePath == "" {
		return nil, status.Error(codes.InvalidArgument, "logical device path is missing")
	}

	fsType := mountCapability.GetFsType()
	if fsType == "" {
		fsType = contract.DefaultFSType
	}
	options, err := contract.DecodeMountOptions(
		request.GetVolumeContext()[contract.AttributeMountOptions],
	)
	if err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}
	options = append(options, mountCapability.GetMountFlags()...)
	if request.GetReadonly() {
		options = appendOption(options, "ro")
	} else {
		options = appendOption(options, "rw")
	}

	if err := os.MkdirAll(request.GetTargetPath(), 0750); err != nil {
		return nil, status.Errorf(codes.Internal, "create target path: %v", err)
	}
	info := mountInfo{
		VolumeType: contract.DirectVolumeType,
		Device:     devicePath,
		FSType:     fsType,
		Options:    options,
	}
	if err := server.addDirectVolume(request.GetTargetPath(), info); err != nil {
		return nil, status.Errorf(codes.Internal, "register DAV metadata: %v", err)
	}

	return &csipb.NodePublishVolumeResponse{}, nil
}

func appendOption(options []string, value string) []string {
	for _, option := range options {
		if option == value {
			return options
		}
	}
	return append(options, value)
}

func (server *Server) NodeUnpublishVolume(
	_ context.Context,
	request *csipb.NodeUnpublishVolumeRequest,
) (*csipb.NodeUnpublishVolumeResponse, error) {
	if request.GetVolumeId() == "" {
		return nil, status.Error(codes.InvalidArgument, "volume ID is missing")
	}
	if request.GetTargetPath() == "" {
		return nil, status.Error(codes.InvalidArgument, "target path is missing")
	}

	if err := server.removeDirectVolume(request.GetTargetPath()); err != nil {
		return nil, status.Errorf(codes.Internal, "remove DAV metadata: %v", err)
	}
	if err := os.RemoveAll(request.GetTargetPath()); err != nil {
		return nil, status.Errorf(codes.Internal, "remove target path: %v", err)
	}
	return &csipb.NodeUnpublishVolumeResponse{}, nil
}

func (server *Server) NodeGetInfo(
	context.Context,
	*csipb.NodeGetInfoRequest,
) (*csipb.NodeGetInfoResponse, error) {
	return &csipb.NodeGetInfoResponse{NodeId: server.nodeID}, nil
}

func (server *Server) NodeGetCapabilities(
	context.Context,
	*csipb.NodeGetCapabilitiesRequest,
) (*csipb.NodeGetCapabilitiesResponse, error) {
	return &csipb.NodeGetCapabilitiesResponse{}, nil
}

func (server *Server) addDirectVolume(targetPath string, info mountInfo) error {
	data, err := json.Marshal(info)
	if err != nil {
		return fmt.Errorf("encode mount info: %w", err)
	}
	directory := filepath.Join(
		server.directVolumeRoot,
		base64.URLEncoding.EncodeToString([]byte(targetPath)),
	)
	if err := os.MkdirAll(directory, 0700); err != nil {
		return fmt.Errorf("create mount-info directory: %w", err)
	}
	if err := os.WriteFile(filepath.Join(directory, mountInfoFileName), data, 0600); err != nil {
		return fmt.Errorf("write mount info: %w", err)
	}
	return nil
}

func (server *Server) removeDirectVolume(targetPath string) error {
	directory := filepath.Join(
		server.directVolumeRoot,
		base64.URLEncoding.EncodeToString([]byte(targetPath)),
	)
	if err := os.RemoveAll(directory); err != nil {
		return fmt.Errorf("remove mount-info directory: %w", err)
	}
	return nil
}
