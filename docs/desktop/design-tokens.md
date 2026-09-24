# Design tokens

The desktop SPA enforces a strict design-token SSOT. This document describes
the rules `apps/desktop/scripts/check-design-tokens.mjs` enforces.

## The rules

1. `src/design-system/tokens.css` is the **only** file allowed to contain
   literal `px` values or hex colours.
2. Every token defined must be consumed somewhere in the app. Unused tokens
   are removed automatically by the gate; nothing is kept "in case we need
   it later".
3. Non-token styles (layout, structural CSS) live in the file that uses them
   and reference tokens via `var(--…)`. They never restate a literal px / hex
   value.

## The gate

```
pnpm design:check        # from repo root or apps/desktop
```

The gate exits `1` and prints a per-file diff if it finds:

- A bare `px`, `rem`, or hex value in a source file that is not
  `tokens.css`.
- A token defined in `tokens.css` but never referenced.
- A token referenced but not defined.

## Design provenance

The visual language draws inspiration from JetBrains IDE density and
hierarchy patterns. That is a design influence only — no JetBrains-licensed
code, asset, or trademark is used. See `THIRD_PARTY_NOTICES.md` §
"Third-party references consulted".

## Token categories

The current set covers:

- **Colour**: surfaces (`--surface-*`), text (`--text-*`), semantic states
  (`--ok`, `--warn`, `--err`), accent.
- **Type**: font families (`--font-sans`, `--font-mono`), sizes
  (`--text-xs` through `--text-3xl`), weights, line heights.
- **Spacing**: an 8-step scale from `--space-1` through `--space-8`.
- **Radii**: `--radius-sm`, `--radius-md`, `--radius-lg`.
- **Shadows**: `--shadow-card`, `--shadow-overlay`.
- **Focus ring**: `--focus-ring-color`, `--focus-ring-offset`,
  `--focus-ring-width`.

Adding a token means adding a definition and at least one consumer in the
same commit.

## Adding a new view

Copy one of the existing view files under `src/views/`. Every view follows
the same shape:

- `<script setup lang="ts">` with local reactive state.
- `<template>` referencing `var(--…)` tokens directly, no scoped literal
  values.
- Scoped styles kept minimal; layout is composed from PanelSection,
  ActivityCard, StatusChip, EmptyState.
- A matching `<ViewName>.test.ts` covering happy path and one error state.

## Design QA screenshots

`pnpm design:qa` runs `scripts/dev/screenshot-views.mjs`, which starts a
headless Chromium and captures five views at three widths (default, narrow,
200% zoom equivalent). Output lands in `.design-review/`. Requires a local
Playwright Chromium install; see the script header for the exact env vars.

## Changing a token

Token changes are visual regressions by definition. Before you push:

1. Run `pnpm design:qa` on your branch.
2. Compare against the main-branch screenshots side by side.
3. Attach the diff to the PR description.

CI does not compare screenshots automatically; the review process is manual.
