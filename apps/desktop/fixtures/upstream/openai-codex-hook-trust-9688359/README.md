# OpenAI Codex Hook trust golden fixture

This fixture is pinned to OpenAI Codex commit
`968835997714baaff199cfed5f89a2c65d8ca77d` and targets Windows behavior.
It uses fixed synthetic absolute paths and commands; it does not read a user's
Codex configuration, trust records, or credentials.

The generator is a minimal independent extraction of these upstream steps:

1. `hooks/src/events/common.rs::matcher_pattern_for_event` keeps matchers only
   for supported event kinds.
2. `hooks/src/engine/discovery.rs::append_matcher_groups` selects
   `commandWindows` on Windows, normalizes timeout/default fields, preserves
   `statusMessage`, and filters `additionalContextLimit`.
3. `hooks/src/engine/discovery.rs::hook_hash` serializes the normalized handler
   through a TOML-representable identity with one handler.
4. `config/src/fingerprint.rs::version_for_toml` converts that value to JSON,
   recursively sorts object keys, emits compact JSON, and hashes it with
   SHA-256.
5. `hooks/src/lib.rs::hook_key` combines the normalized source path, event key,
   group index, and handler index.

Run `node verify.mjs` from this directory to regenerate the cases and compare
stdout byte-for-byte with `expected.json`. The generator and verifier import
only Node built-ins and never call PromptDock's `hook_hash`.

Upstream sources:

- <https://github.com/openai/codex/blob/968835997714baaff199cfed5f89a2c65d8ca77d/codex-rs/hooks/src/events/common.rs>
- <https://github.com/openai/codex/blob/968835997714baaff199cfed5f89a2c65d8ca77d/codex-rs/hooks/src/engine/discovery.rs>
- <https://github.com/openai/codex/blob/968835997714baaff199cfed5f89a2c65d8ca77d/codex-rs/config/src/hook_config.rs>
- <https://github.com/openai/codex/blob/968835997714baaff199cfed5f89a2c65d8ca77d/codex-rs/config/src/fingerprint.rs>
- <https://github.com/openai/codex/blob/968835997714baaff199cfed5f89a2c65d8ca77d/codex-rs/hooks/src/lib.rs>

## Upstream license and notice

Two files in this directory are copied verbatim from the pinned rev, fetched
from the URLs below rather than reconstructed, and pinned by SHA-256:

- `LICENSE.upstream-apache-2.0` —
  <https://raw.githubusercontent.com/openai/codex/968835997714baaff199cfed5f89a2c65d8ca77d/LICENSE>
  — 10926 bytes, SHA-256
  `d17f227e4df5da1600391338865ce0f3055211760a36688f816941d58232d8dc`
- `NOTICE.upstream` —
  <https://raw.githubusercontent.com/openai/codex/968835997714baaff199cfed5f89a2c65d8ca77d/NOTICE>
  — 242 bytes, SHA-256
  `9d71575ecfd9a843fc1677b0efb08053c6ba9fd686a0de1a6f5382fd3c220915`

They carry upstream's own attribution chain, which includes a further MIT grant
(Ratatui) that `NOTICE.upstream` names. They are present because this fixture
documents upstream behaviour — not because any of `generate.mjs`, `cases.json`
or `expected.json` holds upstream bytes; the generator is first-party PromptDock
code and imports only Node built-ins. Verify with:

```sh
sha256sum LICENSE.upstream-apache-2.0 NOTICE.upstream
```
