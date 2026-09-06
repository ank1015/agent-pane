# View image tool

`tool-view-image` reads a file from a selected execution host, validates it, and
provides separate code-mode and model-history delivery paths. Codex behavior is
referenced at commit `ac192cd7937b0d73edc6dffe009940ae53782dd4`.

## Delivery configuration

`ViewImageConfig::delivery` chooses the representation:

- `ImageDelivery::Inline` (default) embeds bytes. Use this for local operation.
- `ImageDelivery::Hosted(Arc<dyn ImagePublisher>)` publishes through an
  application-supplied storage adapter and returns an HTTP(S) URL. Use this with
  a deployment's bucket client. The tool does not create a bucket, own storage
  credentials, fetch published URLs, or fall back to inline on publication errors.

The gateway's provider adapters already support `ImageSource::Url`; no gateway
endpoint or execution-supervisor changes are needed. The model provider fetches
the URL. Hosted results, persisted content, and provider requests contain only
the URL and detail metadata, not base64 or a hidden copy of the source bytes.
The initial execution-host-to-tool read is still needed to validate and prepare
the image; this setting removes subsequent image payload propagation.

The publisher receives validated bytes, their actual MIME type, and a SHA-256
content key. Store those exact bytes immutably, use idempotent publication, and
return a provider-accessible URL. Ownership, access policy, expiration, and
cleanup belong to the application. URLs must remain usable for history replay;
short-lived signed URLs require application-managed renewal before submission.
Do not map a URL to a mutable workspace file. `ViewImageOutput` diagnostics omit inline
bytes and hosted URLs, which may include signatures.

```rust,no_run
use std::sync::Arc;
use execution_core::{ExecutionPath, ExecutionRuntime, OperationContext};
use tool_view_image::{
    ImageDelivery, ImagePublisher, ViewImageConfig, ViewImageInput,
    ViewImageOptions, ViewImageTool,
};

async fn example(
    runtime: &dyn ExecutionRuntime,
    cwd: ExecutionPath,
    publisher: Option<Arc<dyn ImagePublisher>>,
) -> execution_core::ExecutionResult<llm_contracts::ContentPart> {
    let tool = ViewImageTool::new(runtime, cwd, ViewImageConfig {
        delivery: publisher.map_or(ImageDelivery::Inline, ImageDelivery::Hosted),
        // Supply these from the selected model and turn configuration.
        options: ViewImageOptions {
            supports_images: true,
            can_request_original_detail: true,
            unified_image_budget: false,
        },
        ..Default::default()
    })?;
    let content = tool.execute_for_model(&OperationContext::new(), ViewImageInput {
        path: "screenshots/result.png".into(),
        detail: None,
        environment_id: None,
    }).await?;
    // Persist `content` as an image part in the tool result, not as JSON text.
    Ok(content)
}
```

Implement `ImagePublisher::publish` using the application's chosen bucket SDK
or upload service. The async callback receives the operation context and must
honor its cancellation/deadline. A content-addressed object key can use
`asset.sha256` inside an application-owned namespace. The test suite includes
an HTTP object store implementation and verifies retrieval of published bytes.

## Code mode versus model history

`execute` validates the source with the image decoder and preserves its original
bytes. Inline output uses `data:application/octet-stream;base64,...`, exactly as
Codex code mode does. Hosted output instead returns a URL to those original
bytes, uploaded with their real MIME type. This hosted representation is an
intentional configurable extension to Codex's inline contract. GIFs remain GIFs;
large PNGs are not resized in code mode. `code_mode_result()` yields `image_url`
and the effective `detail`, omitting `detail` under unified budgeting.

`execute_for_model` is the direct model-history adapter. It validates the file,
applies Codex prompt-image preparation, and returns `ContentPart::Image` with
an inline base64 source or a hosted URL to the **prepared** image. Invoke it at
history insertion and persist its returned content. Do not run both methods for
one direct call or JSON-stringify code-mode output into a text-only tool result.
The source file is never modified or cached back into the execution filesystem.

Prompt preparation retains the existing Codex algorithms: high detail permits
2,048 pixels per dimension and 2,500 32×32 patches; original detail uses 6,000
pixels and 10,000 patches. PNG/JPEG/WebP pass through when no conversion is
needed; GIF becomes a PNG of its first frame. Resizing uses the Triangle filter;
encoding preserves supported RGB ICC and EXIF metadata. Preparation failures
return structured errors; callers can render them using their tool-error path.

## Model capabilities and schemas

`ViewImageOptions` is caller-controlled; no model-name heuristics are used:

- Models without image input fail before filesystem reads or publication.
- `detail` appears in the input schema only when original detail is supported
  and unified budgeting is disabled. Unsupported `original` requests fall back
  to high detail. Legacy high/original hints remain accepted when hidden.
- Unified budgeting always uses the larger preparation budget, even for a
  legacy high hint, and omits detail from code-mode output and its output schema.
  Model-history content retains the original compatibility hint; Responses Lite
  removes it on the wire. Ordinary OpenAI Responses preserves original detail.
- Defaults support images, disable original requests, and disable unified
  budgeting. The caller resolves unified-budget feature/model eligibility.

Use `tool.definition()` for a function definition with `strict: false` and the
matching input/output schemas. Instance schema methods respect configuration;
free schema helpers use default options or explicit options. Hosted definitions
describe HTTP(S) output, while inline definitions retain Codex's exact wording.
Runtime parsing accepts unknown properties like Codex, despite the advertised
schema's `additionalProperties: false`. `parse_arguments` accepts function-object
arguments and validates image support before decoding them.

Host/environment selection remains caller-owned. This single-host tool rejects
an explicit `environment_id`; select the host first. Existing harness catalogs
remain unchanged. Future registration must pass model options, persist actual
image content parts, and account for the harness's history/commit budgets.
Hosted mode avoids embedding large image data in those commits.

## Reads, paths, and limits

Relative and absolute paths are normalized using the selected host's convention,
including `..` within advertised roots. Root containment, symlink restrictions,
and read-only filesystem policy remain enforced by execution. Reads use bounded
chunks with path, size, revision, offset, and EOF validation. Changed files fail
instead of assembling inconsistent bytes; no retries are automatic.

`max_image_bytes` is an optional application read restriction (unbounded by
package default); `chunk_bytes` defaults to 1 MiB and respects the host read
limit. Prompt preparation separately enforces Codex's 1 GiB input and base64-payload
budgets (without allocating a base64 copy in hosted mode). Transport,
memory, decoder, publisher, and history limits still apply. These are distinct
from model dimensions and patch budgets.

Run `cargo test -p tool-view-image` and
`cargo clippy -p tool-view-image --all-targets -- -D warnings` from `platform/`.
Tests cover remote read faults, raw byte preservation, capability combinations,
prepared dimensions, GIF handling, hosted round trips, compact serialized
history, publisher failures, normalized paths, and final provider image inputs.
