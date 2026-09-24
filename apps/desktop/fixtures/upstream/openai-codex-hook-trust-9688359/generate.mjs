/* global process */
// Minimal independent extraction of the trust-identity path in OpenAI Codex
// source snapshot 968835997714baaff199cfed5f89a2c65d8ca77d.
// This generator intentionally has no dependency on PromptDock Rust code.
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";

const EVENT_KEYS = {
  PreToolUse: "pre_tool_use",
  PermissionRequest: "permission_request",
  PostToolUse: "post_tool_use",
  PreCompact: "pre_compact",
  PostCompact: "post_compact",
  SessionStart: "session_start",
  SessionEnd: "session_end",
  UserPromptSubmit: "user_prompt_submit",
  SubagentStart: "subagent_start",
  SubagentStop: "subagent_stop",
  Stop: "stop",
  Interrupt: "interrupt"
};

const MATCHER_EVENTS = new Set([
  "PreToolUse",
  "PermissionRequest",
  "PostToolUse",
  "PreCompact",
  "PostCompact",
  "SessionStart",
  "SessionEnd",
  "SubagentStart",
  "SubagentStop"
]);

const CONTEXT_EVENTS = new Set([
  "PreToolUse",
  "PostToolUse",
  "SessionStart",
  "UserPromptSubmit",
  "SubagentStart"
]);

const DEFAULT_CONTEXT_LIMIT = 2500;

function normalizeTimeout(eventName, timeout) {
  if (eventName === "SessionEnd" || eventName === "Interrupt") {
    return Math.min(3, Math.max(1, timeout ?? 1));
  }
  return Math.max(1, timeout ?? 600);
}

function normalizeIdentity(testCase, platform) {
  const source = testCase.handler;
  if (source.type !== "command") throw new Error(`${testCase.id}: command case required`);
  const command = platform === "windows" ? source.commandWindows ?? source.command : source.command;
  if (typeof command !== "string" || command.trim() === "") {
    throw new Error(`${testCase.id}: empty command`);
  }
  const handler = {
    type: "command",
    command,
    timeout: normalizeTimeout(testCase.eventName, source.timeout),
    async: source.async ?? false
  };
  if (source.statusMessage !== undefined) handler.statusMessage = source.statusMessage;
  if (
    CONTEXT_EVENTS.has(testCase.eventName) &&
    source.additionalContextLimit !== undefined &&
    source.additionalContextLimit !== DEFAULT_CONTEXT_LIMIT
  ) {
    handler.additionalContextLimit = source.additionalContextLimit;
  }
  const identity = {
    event_name: EVENT_KEYS[testCase.eventName]
  };
  if (MATCHER_EVENTS.has(testCase.eventName) && testCase.matcher !== undefined) {
    identity.matcher = testCase.matcher;
  }
  identity.hooks = [handler];
  assertTomlRepresentable(identity, testCase.id);
  return identity;
}

function assertTomlRepresentable(value, path) {
  if (value === null || value === undefined) throw new Error(`${path}: null is not TOML-representable`);
  if (typeof value === "number" && (!Number.isSafeInteger(value) || !Number.isFinite(value))) {
    throw new Error(`${path}: number is not a TOML integer`);
  }
  if (Array.isArray(value)) {
    value.forEach((item, index) => assertTomlRepresentable(item, `${path}[${index}]`));
  } else if (typeof value === "object") {
    Object.entries(value).forEach(([key, item]) => assertTomlRepresentable(item, `${path}.${key}`));
  }
}

function canonical(value) {
  if (Array.isArray(value)) return value.map(canonical);
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.keys(value)
        .sort()
        .map((key) => [key, canonical(value[key])])
    );
  }
  return value;
}

function hashIdentity(identity) {
  const canonicalJson = JSON.stringify(canonical(identity));
  return {
    canonicalJson,
    trustedHash: `sha256:${createHash("sha256").update(canonicalJson).digest("hex")}`
  };
}

const input = JSON.parse(readFileSync(new URL("./cases.json", import.meta.url), "utf8"));
const cases = input.cases.map((testCase) => {
  const normalizedIdentity = normalizeIdentity(testCase, input.platform);
  const { canonicalJson, trustedHash } = hashIdentity(normalizedIdentity);
  return {
    id: testCase.id,
    key: `${testCase.sourcePath}:${EVENT_KEYS[testCase.eventName]}:${testCase.groupIndex}:${testCase.handlerIndex}`,
    normalizedIdentity,
    canonicalJson,
    trustedHash
  };
});

process.stdout.write(
  `${JSON.stringify(
    {
      sourceSha: input.sourceSha,
      platform: input.platform,
      generator: "generate.mjs (independent extraction; no PromptDock imports)",
      cases
    },
    null,
    2
  )}\n`
);
