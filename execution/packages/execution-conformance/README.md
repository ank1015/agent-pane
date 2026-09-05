# execution-conformance

Reusable black-box behavioral checks for implementations of
`execution_core::ExecutionRuntime`.

The suite verifies the common contract rather than implementation details. It
covers:

- descriptor and root invariants;
- bounded binary filesystem reads;
- conditional, atomic, and idempotent writes;
- bounded directory listing and removal;
- argv process execution, environment construction, and stdout/stderr capture;
- ordered cursor-based output replay;
- idempotent process start and process-input writes;
- bounded process reads;
- PTY input and resize when advertised;
- explicit signals, termination, and process timeouts; and
- supervisor-generation mismatch handling.

Target-specific security tests that require creating native symlinks outside an
execution root remain in the supervisor-core test suite. Provider lifecycle and
transport-fault injection remain in provider-specific tests.

```rust,no_run
use execution_conformance::{ConformanceConfig, run_all};
use execution_core::ExecutionRuntime;

# async fn check(runtime: &dyn ExecutionRuntime) -> Result<(), Box<dyn std::error::Error>> {
let config = ConformanceConfig::for_runtime(runtime)?;
let report = run_all(runtime, &config).await?;
assert!(!report.passed_checks().is_empty());
# Ok(())
# }
```

The default configuration selects `/bin/sh` for Unix hosts and Windows
PowerShell for Windows hosts. Callers may override the command executable and
arguments for another native environment.
