# E2B CPU/RAM resource tiers

Research date: 2026-09-05

## Implemented outcome

The selected production tiers are 1024 MiB/1 vCPU, 2048 MiB/2 vCPU (default),
4096 MiB/2 vCPU, and 8192 MiB/4 vCPU. The current templates were built and
published successfully on 2026-09-07 with supervisor release
`0.1.0-dev-7d4c8ad2` and `ripgrep`. Live sandboxes from every tier passed the
complete `execution-supervisor` operation surface and were then deleted.

The earlier 512-MiB exploration and suggested mapping below are retained as the
research record that informed the final selection.

## Conclusion

E2B assigns CPU and RAM to a **template build**, not to an individual
`Sandbox.create` call. The current create-sandbox request accepts a template or
snapshot ID but has no CPU or RAM override. Therefore, using one execution base
template per resource tier is the correct design.

The proposed RAM values `512`, `1024`, `2048`, and `8192` MiB are all valid for
standard E2B-hosted projects. Standard plans document up to 8 vCPUs and 8 GiB;
larger project-specific ceilings are available through E2B support. The public
implementation validates CPU as `1` or an even number and RAM as an even MiB
value, subject to the project's effective `maxVcpu` and `maxRamMb` limits.

Snapshots are different: E2B records the source sandbox's vCPU and RAM on the
snapshot build, and sandbox creation restores the selected build's values. A
gateway RAM input must consequently be valid only for base-template creation,
never snapshot creation.

## Supported values

E2B's current hosted-product documentation gives these standard ceilings:

| Resource | Hobby | Pro | Relevant rule |
| --- | ---: | ---: | --- |
| vCPU | 8 | 8+ | Hosted choices within the standard ceiling are 1 or an even number (`1, 2, 4, 6, 8`) |
| RAM | 8 GiB | 8+ GiB | Even MiB values; all four proposed tiers are accepted |

The limits above come from E2B's [Billing & limits](https://docs.e2b.dev/billing)
page. E2B's public API schema specifies a minimum of 1 for `cpuCount` and 128
MiB for `memoryMB`; its server-side validation additionally requires CPU to be
1 or even and memory to be divisible by 2, then checks the project's limits
([`limits.go`](https://github.com/e2b-dev/infra/blob/04db4f13610e7c73927648b91b154ee220ca6dc5/packages/api/internal/team/limits.go#L13-L80)).
The platform-level server guard is 32 vCPUs, but that is not the hosted project's
entitlement; the effective project limit wins.

CPU and RAM are validated independently, so E2B does not impose a CPU-to-RAM
ratio. Any allowed CPU value can be combined with any allowed RAM value. The
gateway can choose its own fixed mapping.

## Suggested initial mapping

This mapping preserves the current execution template at 1 GiB/2 vCPU, gives
the memory tiers useful differentiation, and remains within every documented
standard project ceiling:

| Gateway RAM option | Template vCPU | Approx. compute cost/hour | Note |
| ---: | ---: | ---: | --- |
| 512 MiB | 1 | $0.0585 | Valid, but should remain experimental until the supervisor plus representative workloads are tested under this ceiling |
| 1024 MiB | 2 | $0.1170 | Current execution-base specification; best default |
| 2048 MiB | 2 | $0.1332 | More memory without paying for more CPU |
| 8192 MiB | 4 | $0.3312 | Balanced larger tier; use 2 vCPU instead ($0.2304/hour) if workloads are known to be RAM-heavy rather than CPU-heavy |

The calculation uses E2B's published rates of $0.000014/vCPU/second and
$0.0000045/GiB/second from the [pricing page](https://e2b.dev/pricing).

Whether to include 512 MiB is a product decision, not an E2B limitation. E2B's
generic default sandbox is documented as 2 vCPU/512 MiB, so the size is
supported. A short probe of this repository's 1-GiB template with the real
supervisor running showed 976 MiB visible to the guest, 179 MiB used, 796 MiB
available, no swap, and about 2.4 MiB RSS for the idle supervisor itself. This
shows that the supervisor is not the reason to reject 512 MiB, but it does not
establish that package installation, compilation, or agent workloads are
reliable at that ceiling. A conservative first release can omit 512 and add it
after representative OOM/health-check tests.

## Template build inputs and commands

The current CLI command is `e2b template create`; `e2b template build` is
deprecated. The CLI accepts `--cpu-count` and `--memory-mb` and defaults to 2
vCPU/1024 MiB for a custom build. See the official
[CLI template reference](https://docs.e2b.dev/sdk-reference/cli/v2.16.1/template)
and [template build guide](https://docs.e2b.dev/template/build).

No builds were run during this research. The proposed commands are:

```sh
e2b template create agent-pane-execution-base-512mb \
  --path execution/templates/e2b-supervisor-base \
  --dockerfile Dockerfile \
  --cpu-count 1 \
  --memory-mb 512

e2b template create agent-pane-execution-base-1gb \
  --path execution/templates/e2b-supervisor-base \
  --dockerfile Dockerfile \
  --cpu-count 2 \
  --memory-mb 1024

e2b template create agent-pane-execution-base-2gb \
  --path execution/templates/e2b-supervisor-base \
  --dockerfile Dockerfile \
  --cpu-count 2 \
  --memory-mb 2048

e2b template create agent-pane-execution-base-8gb \
  --path execution/templates/e2b-supervisor-base \
  --dockerfile Dockerfile \
  --cpu-count 4 \
  --memory-mb 8192
```

After validation, each can be published with:

```sh
e2b template publish <template-name> --yes
```

The equivalent build API is `POST /v3/templates` with camelCase `cpuCount` and
`memoryMB` fields; see E2B's
[Create template (v3)](https://docs.e2b.dev/api-reference/templates/create-template-v3)
reference. These fields belong to the template-build request. They are absent
from the [create-sandbox request](https://docs.e2b.dev/api-reference/sandboxes/create-sandbox),
which accepts `templateID` instead.

## Snapshot resource behavior

E2B documents that a snapshot captures a running sandbox's filesystem and
memory and that its snapshot ID is passed directly to `Sandbox.create` to
restore that state ([Sandbox snapshots](https://docs.e2b.dev/sandbox/snapshots)).
The public server source makes the resource behavior explicit:

- Snapshot creation stores `Vcpu: sbx.VCpu` and `RamMb: sbx.RamMB` and pins the
  snapshot to the source build
  ([`pause_instance.go`](https://github.com/e2b-dev/infra/blob/04db4f13610e7c73927648b91b154ee220ca6dc5/packages/api/internal/orchestrator/pause_instance.go#L139-L172)).
- Sandbox creation always supplies `RamMb` and `Vcpu` from the selected build,
  including snapshot builds
  ([`create_instance.go`](https://github.com/e2b-dev/infra/blob/04db4f13610e7c73927648b91b154ee220ca6dc5/packages/api/internal/orchestrator/create_instance.go#L332-L347)).

This is also why a snapshot cannot safely be restored with a different machine
shape: Firecracker restores VM and memory state captured with the original
topology.

## Account and runtime inspection

The installed CLI is `2.18.0`. Read-only commands showed:

- Selected project: `Sugar's Project`.
- Current execution base template: 2 vCPU, 1024 MiB.
- An existing project template: 2 vCPU, 8192 MiB.
- Existing paused sandboxes include 2-vCPU/8192-MiB instances.

This proves the project accepts at least 8192 MiB RAM with 2 vCPU. The CLI does
not expose effective project `maxVcpu`/`maxRamMb` values. The dashboard limits
endpoint required reauthentication, so no login or token refresh was performed.
Thus the exact custom account ceiling was not verified. The suggested mapping
uses no more than 4 vCPU and 8 GiB, which is within the documented standard
Hobby and Pro ceilings.

Read-only commands used:

```sh
e2b --version
e2b auth info
e2b template create --help
e2b template list --format json
e2b sandbox list --state running --format json
e2b sandbox list --state paused --format json
```

One temporary sandbox was then created from the existing 1-GiB execution base,
the real `execution-supervisor serve` process was started, and guest memory/RSS
were inspected. It was immediately killed and a final running-sandbox listing
was empty. No template, snapshot, or account setting was created or changed.
