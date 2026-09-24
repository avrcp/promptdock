# Third-party notices

This file lists every bundled third-party work that requires attribution, and
records which of them ship under a license compatible with the project's own
license. The PromptDock project is licensed under the
**Apache License, Version 2.0**; see `LICENSE` at the repo root. Third-party
work keeps its own terms, and nothing in this file grants those terms to
PromptDock's sources or narrows what Apache-2.0 grants.

## Bundled code with third-party license obligations

### `crates/wechat-ilink` — WeChat iLink client

- Upstream: `https://github.com/Tencent/openclaw-weixin` (npm:
  `@tencent-weixin/openclaw-weixin`)
- Upstream version: 2.4.6 (upstream commit `cef0bfc390393f716903e16d50408118047f87e0`)
- Copyright: `Copyright (C) 2026 Tencent. All rights reserved.`
- License: MIT
- License text pinned at:
  `https://github.com/Tencent/openclaw-weixin/blob/cef0bfc390393f716903e16d50408118047f87e0/LICENSE`

`crates/wechat-ilink` is a protocol-compatibility implementation written
against that pinned reference. Its protocol constants, wire shapes and
transport semantics follow Tencent's client; the Rust module split, tests and
fixtures are PromptDock's own, and no upstream source bytes are vendored. The
Tencent copyright and the MIT text are reproduced below because MIT requires
its notice to travel with all copies and substantial portions of the licensed
work.

### Complete LICENSE text from the pinned reference

Tencent is pleased to support the open source community by making
openclaw-weixin available.

Copyright (C) 2026 Tencent. All rights reserved.

openclaw-weixin is licensed under the MIT.

Terms of the MIT:

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

The crate itself is first-party PromptDock code and is distributed under the
project's Apache-2.0 license, which its `Cargo.toml` declares. The two grants
are compatible: MIT places no restriction on the license you pick for your own
independent implementation, and it neither extends to the rest of this
monorepo nor grants any right to Tencent names, marks or logos.

### `apps/desktop/fixtures/upstream/openai-codex-hook-trust-9688359/`

- Upstream: `https://github.com/openai/codex` (rev `968835997714baaff199cfed5f89a2c65d8ca77d`)
- License (upstream): Apache-2.0
- Carried beside the fixture: `LICENSE.upstream-apache-2.0`, `NOTICE.upstream`

This is a **behavioural compatibility fixture** — it does not copy upstream
source bytes. The fixture generator is first-party PromptDock. It asserts that
our Codex Hook trust handling matches the upstream protocol shape so a future
Codex release can be swapped in without re-writing tests.

Upstream's Apache-2.0 license text and its own `NOTICE` are copied verbatim into
the fixture directory, fetched from the pinned rev and pinned there by SHA-256
rather than reconstructed. `NOTICE.upstream` carries upstream's own attribution
chain, which includes a further MIT grant (Ratatui), and that travels with it.
No upstream source code is redistributed: the generator, cases and expectations
are first-party, and importing upstream bytes later would need this section
updated again.

### Fonts (OFL-1.1)

Two self-hosted variable fonts ship under the SIL Open Font License 1.1.
Font binaries live in the frontend asset directories; the OFL text is
reproduced below.

- `@fontsource-variable/geist` — Geist Variable, by Vercel
- `@fontsource-variable/jetbrains-mono` — JetBrains Mono Variable, by
  Philipp Nurullin and collaborators

SIL OPEN FONT LICENSE Version 1.1 — 26 February 2007. Preamble and terms
available at https://openfontlicense.org/open-font-license-official-text/.
The fonts may be used in a work regardless of that work's own license. They
may not be sold by themselves, and modified versions must not use the
Reserved Font Name.

## Third-party references consulted (no code copy)

### JetBrains "ThinkRail" (design language)

The admin console UI was designed against a JetBrains-flavoured brief. That
brief informed spacing / hierarchy / density choices but no JetBrains code,
assets, or trademarked brand are used. No JetBrains license obligation
attaches to this repo as a result.

### TencentCloud Octop

Architectural prior art consulted when designing the transport abstraction
between `relay-transport-gateway` and `relay-transport-http`. No code copy.
No license obligation.

## Package dependencies

Every runtime and dev dependency from pnpm and Cargo ships under its own
upstream license. The committed lockfiles at the repo root (`pnpm-lock.yaml`,
`Cargo.lock`) enumerate the exact versions.

When you redistribute a build of this project, comply with each dependency's
own license terms. Apache-2.0 dependencies require that you carry their
`NOTICE` files, and MPL / LGPL dependencies may require that you offer
source for those libraries specifically.

## Contributor license agreements

We do not currently require a CLA. Contributors retain copyright of their own
contributions and license them to this project under Apache-2.0, the project's
license. This document will be updated if that policy changes.

## What Apache-2.0 obliges you to carry

When you redistribute PromptDock sources or a build derived from them, section
4 of the license requires that you:

- keep this `THIRD_PARTY_NOTICES.md` file and the `LICENSE` file;
- carry the upstream notices listed above (Tencent copyright + MIT text for the
  WeChat iLink client, the OpenAI Apache-2.0 text and `NOTICE.upstream` beside
  the Codex fixture, the OFL statement for the two fonts);
- state, in files you modify, that you changed them;
- pass along any `NOTICE` file that ships with an Apache-2.0 dependency. No
  such file is bundled as source in this repository; dependency `NOTICE` text
  is handled by your dependency tooling and release packaging. (`NOTICE.upstream`
  belongs to a reference project we cite, not to a packaged dependency, and is
  listed in the bullet above.)

We deliberately do not inject per-file SPDX headers across the tree. Adding a
header to every file would re-write otherwise unmodified upstream-derived
sources for no compliance gain, since the root `LICENSE`, the manifest
`license` fields and this document already identify the terms.
