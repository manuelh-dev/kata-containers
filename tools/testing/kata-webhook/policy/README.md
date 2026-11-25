# Static Agent policy

[`static-policy.rego`](static-policy.rego) is a Pod-independent Agent policy
that the init-data webhook can inject instead of a genpolicy-generated policy.
This page lists how this helps handle each Agent ttRPC request under the new
agent security policy design, and how that compares with genpolicy.

The list below needs more detailed human assessment on the correctness of claims
and is supposed to be a work tracker for now.

The 42 Agent ttRPC methods use 41 request types (`ReadStdout` and `ReadStderr`
share `ReadStreamRequest`).

Note that, the Health service methods `Check` and `Version` are not mediated
by the Agent policy (see `src/libs/protocols/protos/health.proto`), currently
out of consideration here.

## Requests with rules in the static policy

| Request | Genpolicy parity | Notes |
| --- | --- | --- |
| `CreateSandbox` | Changed: stricter | Adds checks for a 64-hex sandbox ID, the hostname shape, and no control characters in DNS entries. The remaining checks match genpolicy. The in-guest watchdog may police it in addition or in replacement in the future. |
| `UpdateInterface` | Parity | Same rule, with the genpolicy settings values inlined. |
| `UpdateRoutes` | Parity | Same rule, with the genpolicy settings values inlined. |
| `AddARPNeighbors` | Parity | Same rule, with the genpolicy settings values inlined. |

## Requests allowed by default and enforced elsewhere

| Request | Genpolicy parity | Enforced by |
| --- | --- | --- |
| `CreateContainer` | Changed: genpolicy matches it against the per-Pod policy | The external in-guest container watchdog |
| `CopyFile` | Changed: genpolicy checks path patterns and symlinks | The Agent, which confines files to the Kata shared directory. Trusting the Agent is a decision for now. Current work is ongoing to even deprecate CopyFile. |
| `RemoveContainer` | Changed: genpolicy tracks state per policy container slot | The Agent, which rejects unknown IDs. Upstream genpolicy now also allows unknown IDs (commit `0ec6e9b663`). |
| `StartContainer` | Parity | The Agent rejects unknown container IDs |
| `SignalProcess` | Parity | The Agent rejects unknown container IDs |
| `WaitProcess` | Parity | The Agent rejects unknown process IDs |
| `StatsContainer` | Parity | The Agent rejects unknown container IDs |
| `TtyWinResize` | Parity | The Agent rejects unknown process IDs |

The Agent ID checks only confirm that a container or process exists; they are
not authorization.

## Requests allowed by default and not enforced elsewhere

| Request | Genpolicy parity | Notes |
| --- | --- | --- |
| `DestroySandbox` | Parity | The request has no ID; it only affects availability, which the host controls anyway. |
| `GetOOMEvent` | Parity | No input |
| `GetGuestDetails` | Parity | Read-only |
| `RemoveStaleVirtiofsShareMounts` | Parity | No input |
| `OnlineCPUMem` | Parity | Only affects availability |

## Requests denied by default

| Request | Genpolicy parity | Notes |
| --- | --- | --- |
| `ExecProcess` | Changed: denied | Genpolicy allows the Pod's exec probes and lifecycle-hook commands. Denied for now; to be decided later, likely to use the watchdog component. |
| `UpdateContainer` | Parity | A candidate for the in-guest watchdog. The Agent applies the new resources to the container's cgroups without rewriting its `config.json`, so a watchdog that validates `config.json` does not see the change. Denied for now. |
| `UpdateEphemeralMounts` | Parity | Genpolicy denies it unless its settings enable it. Like `UpdateContainer`, a candidate for the in-guest watchdog; denied for now. |
| `SetPolicy` | Parity | |
| `PauseContainer` | Parity | |
| `ResumeContainer` | Parity | |
| `WriteStdin` | Parity | Genpolicy denies it unless its settings enable it. |
| `ReadStdout` / `ReadStderr` | Parity | Genpolicy denies it unless its settings enable it. Output is redacted, so `kubectl logs` is empty. |
| `CloseStdin` | Parity | Genpolicy denies it unless its settings enable it. |
| `GetDiagnosticData` | Parity | Genpolicy denies it unless its settings enable it. |
| `GetMetrics` | Parity | |
| `ListInterfaces` | Parity | |
| `ListRoutes` | Parity | |
| `GetIPTables` | Parity | |
| `SetIPTables` | Parity | |
| `MemAgentMemcgSet` | Parity | |
| `MemAgentCompactSet` | Parity | |
| `MemHotplugByProbe` | Parity | |
| `SetGuestDateTime` | Parity | |
| `ReseedRandomDev` | Parity | |
| `AddSwap` | Parity | |
| `AddSwapPath` | Parity | |
| `GetVolumeStats` | Parity | |
| `ResizeVolume` | Parity | |
