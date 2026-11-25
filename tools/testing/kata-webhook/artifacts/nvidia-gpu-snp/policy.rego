# Copyright (c) NVIDIA Corporation
#
# SPDX-License-Identifier: Apache-2.0
#
# Pod-independent Agent policy that the init-data webhook can inject instead of
# a genpolicy-generated, per-Pod policy.

package agent_policy

import future.keywords.in
import future.keywords.every
import future.keywords.if

default AllowRequestsFailingPolicy := false

# Validated by the external in-guest container watchdog, not by this policy.
# Without that watchdog, this allows the host to run arbitrary containers.
default CreateContainerRequest := true

# The Agent confines copied files to the Kata shared directory. Their contents
# remain untrusted host input.
default CopyFileRequest := true

# Lifecycle and read-only requests. The Agent rejects unknown container IDs.
default DestroySandboxRequest := true
default GetOOMEventRequest := true
default GuestDetailsRequest := true
default OnlineCPUMemRequest := true
default RemoveContainerRequest := true
default RemoveStaleVirtiofsShareMountsRequest := true
default SignalProcessRequest := true
default StartContainerRequest := true
default StatsContainerRequest := true
default TtyWinResizeRequest := true
default WaitProcessRequest := true

# Allowed only when the rules below match.
default AddARPNeighborsRequest := false
default CreateSandboxRequest := false
default UpdateInterfaceRequest := false
default UpdateRoutesRequest := false

# Workload-specific requests, denied until they are validated in the guest.
default ExecProcessRequest := false
default UpdateContainerRequest := false
default UpdateEphemeralMountsRequest := false

# The host must not replace this policy.
default SetPolicyRequest := false

# Denied host-controlled operations, data access and stream access.
default AddSwapPathRequest := false
default AddSwapRequest := false
default CloseStdinRequest := false
default GetDiagnosticDataRequest := false
default GetIPTablesRequest := false
default GetMetricsRequest := false
default ListInterfacesRequest := false
default ListRoutesRequest := false
default MemAgentCompactConfig := false
default MemAgentMemcgConfig := false
default MemHotplugByProbeRequest := false
default PauseContainerRequest := false
default ReadStreamRequest := false
default ReseedRandomDevRequest := false
default ResizeVolumeRequest := false
default ResumeContainerRequest := false
default SetGuestDateTimeRequest := false
default SetIPTablesRequest := false
default VolumeStatsRequest := false
default WriteStreamRequest := false

SANDBOX_STORAGES := [
    {
        "driver": "ephemeral",
        "driver_options": [],
        "source": "shm",
        "fstype": "tmpfs",
        "options": [
            "noexec",
            "nosuid",
            "nodev",
            "mode=1777",
            "size=67108864",
        ],
        "mount_point": "/run/kata-containers/sandbox/shm",
        "fs_group": null,
        "shared": false,
    },
]

FORBIDDEN_DEVICE_NAMES := ["lo"]

IPV4_LOOPBACK_REGEX := "^127\\.(?:[0-9]{1,3}\\.){2}[0-9]{1,3}$"
IPV6_LOOPBACK_REGEX := "^(?:0{0,4}:){0,7}0{0,3}1$"

# Hostname and DNS remain untrusted host input. Only their shape is checked.
CreateSandboxRequest := {"ops": [{"op": "add", "path": "/pstate/sandbox_created", "value": true}], "allowed": true} if {
    print("CreateSandboxRequest: input =", input)

    not data.pstate.sandbox_created

    regex.match("^[a-f0-9]{64}$", input.sandbox_id)
    regex.match("^[A-Za-z0-9]([A-Za-z0-9.-]{0,61}[A-Za-z0-9])?$", input.hostname)
    every i_dns in input.dns {
        not regex.match("[\\x00-\\x1f]", i_dns)
    }

    count(input.guest_hook_path) == 0
    count(input.kernel_modules) == 0
    input.sandbox_pidns == false

    every i_storage in input.storages {
        i_storage in SANDBOX_STORAGES
    }

    print("CreateSandboxRequest: true")
}

UpdateInterfaceRequest if {
    print("UpdateInterfaceRequest: input =", input)

    i_interface := input.interface

    # Typically, just IFF_NOARP is used.
    bits.and(i_interface.raw_flags, bits.negate(128)) == 0
    not i_interface.name in FORBIDDEN_DEVICE_NAMES
    i_interface.hwAddr != "00:00:00:00:00:00"

    print("UpdateInterfaceRequest: true")
}

UpdateRoutesRequest if {
    print("UpdateRoutesRequest: input =", input)

    every i_route in input.routes.Routes {
        not regex.match(IPV4_LOOPBACK_REGEX, i_route.source)
        not regex.match(IPV6_LOOPBACK_REGEX, i_route.source)
        not i_route.device in FORBIDDEN_DEVICE_NAMES
    }

    print("UpdateRoutesRequest: true")
}

AddARPNeighborsRequest if {
    print("AddARPNeighborsRequest: input =", input)

    every i_neigh in input.neighbors.ARPNeighbors {
        not i_neigh.device in FORBIDDEN_DEVICE_NAMES
        i_neigh.toIPAddress.mask == ""
        not regex.match(IPV4_LOOPBACK_REGEX, i_neigh.toIPAddress.address)
        i_neigh.state in [2, 128]
        bits.or(i_neigh.flags, 136) == 136
    }

    print("AddARPNeighborsRequest: true")
}
