# Codex Hook integration

PromptDock attaches to Codex desktop through the standard Codex Hook
mechanism. Nothing about Codex itself is patched, proxied, or intercepted.

## Which events

PromptDock registers for three hook boundaries:

- `UserPromptSubmit` — fires when the user submits a prompt. PromptDock
  records a `run_started` event.
- `Stop` — fires when the assistant finishes a turn. PromptDock records
  `output_produced` (FullFinal text) and `run_settling`.
- `PermissionRequest` — optional. When enabled, PromptDock records an
  "attention needed" event so the user gets a notification on their phone.

`PermissionRequest` is intentionally optional: the desktop host does not act
on it. It is a signal, never an approval.

## Where the hook points live

On Windows, Codex desktop reads hooks from `%USERPROFILE%\.codex\hooks.json`
(and a companion `hooks.state` TOML block). The desktop host's
`hook_installer` module owns the add/update/remove logic. It writes exactly
one hook entry per event, keyed by its own identifier, and does not touch
other entries.

The command installed for each hook is a `--capture-agent-event` mode of the
portable `PromptDockDesktop.exe`. It reads the hook's JSON payload from stdin
and appends a protected record to the desktop's local inbox file.

## Neutral-output contract

Every `--capture-agent-event` invocation returns exit code 0 and either an
empty stdout or `{}` on stdout. Non-zero exits or stdout that Codex does not
expect will confuse the desktop's hook runner. Stderr carries diagnostic
codes (`CAPTURE_STDIN_TIMEOUT`, `HOOK_INPUT_TOO_LARGE`,
`RUNTIME_RESET_INCOMPLETE`) and is logged to the desktop's error file, not to
Codex.

## Trust verification

`apps/desktop/fixtures/upstream/openai-codex-hook-trust-9688359/` is a
behavioural fixture extracted from `openai/codex` rev `9688359…` under
Apache-2.0 upstream terms. It asserts that the trust handshake between our
hook installer and Codex matches upstream expectations. This is not a copy
of upstream code — see `THIRD_PARTY_NOTICES.md`.

## Installing

The desktop host offers "install hooks" and "uninstall hooks" from its
tray menu. Installation writes only to files you can also edit manually;
there is no daemon, no service, no kernel driver.

Do **not** attempt to install real Codex hooks against a live desktop
install from a public CI run. The fixture under `fixtures/upstream/` exists
precisely because the source environment does its end-to-end acceptance
inside a controlled sandbox.

## Reset

If you uninstall PromptDock, run `clear-local-data.cmd` first (see
[../desktop/local-data.md](../desktop/local-data.md)). It removes the local
inbox and runtime DB while preserving `capture-policy.json`. Then invoke
"uninstall hooks" from the tray. The hook entries reference the executable
by absolute path; leaving stale hook entries pointing at a deleted executable
will make Codex's hook runner fail noisily on every prompt.
