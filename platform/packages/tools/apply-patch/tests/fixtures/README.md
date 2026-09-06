These unmodified fixtures are copied from OpenAI Codex, `codex-rs/apply-patch/tests/fixtures/scenarios`, at commit `ac192cd7937b0d73edc6dffe009940ae53782dd4` (Apache-2.0; see LICENSE and NOTICE).

`apply_patch.lark` is copied from `codex-rs/core/assets/tools/apply_patch.lark` at the same commit to pin the model-facing contract.

The integration test compares complete filesystem trees and bytes. Scenario 015 is intentionally excluded: it tests standalone CLI partial failure before a missing delete; Codex model-tool verification rejects that patch before any writes. The local integration suite tests the model-tool preflight and partial-mutation behavior separately. CRLF fixture bytes must be preserved.
