# Relay Admin design system

This directory is the single visual foundation for the Relay Admin application. It implements the dark operator workbench described by the repository's `DESIGN.md` and keeps page code focused on product behavior.

## Import order

`main.ts` imports the layers once and in this order:

1. `fonts.css` loads the self-hosted Geist and JetBrains Mono assets.
2. `tokens.css` defines primitives, semantic `--pd-*` tokens, responsive values, reduced-motion values, and forced-colors values.
3. `reset.css` establishes the document baseline and accessible focus treatment.
4. `utilities.css` provides a small set of composition and typography helpers.
5. `motion.css` provides the approved reveal, drawer, press-feedback, and progress motion primitives.

Component-scoped styles must consume semantic `--pd-*` tokens. Do not copy palette values or create page-local spacing, radius, shadow, or transition systems.

## Typography and Chinese content

Geist Variable is the Latin UI face and JetBrains Mono Variable is reserved for IDs, ports, URLs, timestamps, counts, and other machine-readable values. The sans stack explicitly falls back through Noto Sans CJK SC, Noto Sans SC, PingFang SC, and Microsoft YaHei so mixed Chinese and Latin copy remains legible. Do not force letter spacing on Chinese body copy; the metadata utility is for short labels only.

## Motion and accessibility

Motion is brief and functional: opacity plus a 4px reveal, compact drawer transitions, direct press feedback, and a progress spinner. All transition durations use `--pd-ease-standard` (`cubic-bezier(0.2, 0, 0, 1)`) for a consistent deceleration curve. Pressable controls (buttons, icon buttons, menu triggers, filter toggles) shift 1px on `:active` when the user has no reduced-motion preference. Reduced-motion values collapse ordinary transitions to zero, disable continuous spinning, and suppress press transforms. Focus rings use `--pd-border-focus`; forced-colors overrides map the semantic layer to system colors.

## Border semantics

Three semantic border tokens cover every border in the UI. Do not introduce new border tokens or use palette values directly.

| Token                   | Value                             | Use for                                                                                                                                                         |
| ----------------------- | --------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `--pd-border-separator` | `var(--pd-palette-hover)`         | Shell seams (topbar bottom, sidebar right), card/panel internal borders, table row/column dividers, panel header bottoms, list item separators, menu separators |
| `--pd-border-emphasis`  | `var(--pd-palette-border-strong)` | Selected/active controls, kbd keycaps, scrollbar thumb, branding marks — any non-overlay element that needs a strong outline                                    |
| `--pd-border-overlay`   | `var(--pd-palette-border-strong)` | Dialog, Drawer, CredentialReceiptDialog, and Menu popup outer boundaries                                                                                        |

`--pd-border-focus` remains reserved for focus rings. Forced-colors overrides map all three semantic tokens to `CanvasText` and focus to `Highlight`.

## Component size tokens

| Token                             | Value | Purpose                                                        |
| --------------------------------- | ----- | -------------------------------------------------------------- |
| `--pd-keycap-height`              | 22px  | `<kbd>` keycap height and min-width                            |
| `--pd-badge-height`               | 26px  | Topbar environment badge height                                |
| `--pd-status-chip-height`         | 22px  | StatusChip and system flag inline height                       |
| `--pd-icon-button-size-sm`        | 28px  | Small icon button (close buttons, compact actions)             |
| `--pd-icon-button-size-md`        | 32px  | Medium icon button (default)                                   |
| `--pd-control-height-touch`       | 40px  | Touch-target override for coarse pointers and narrow viewports |
| `--pd-control-height-md`          | 32px  | Standard control height                                        |
| `--pd-empty-state-min-block-size` | 132px | EmptyState minimum block size                                  |
| `--pd-drawer-max-width`           | 420px | Drawer configured-width cap                                    |
| `--pd-dialog-max-width`           | 448px | Dialog max-width                                               |

AppIconButton uses `flex: 0 0 auto` on its root to prevent flex growth. On `pointer: coarse` or viewports at most 767px wide, both sm and md sizes escalate to `--pd-control-height-touch`.

## Governance

Run `pnpm --filter @promptdock/relay-admin design:check` after changing UI styles. The guard blocks new raw colors outside the token authority, `transition: all`, radii above 8px except documented true pills, shadows outside approved overlay components, deleted border tokens (`--pd-border-muted`, `--pd-border-default`, `--pd-border-strong`), bare role heights, unauthorized `--pd-text-hint` usage, legacy `.numeric`, feature viewport magic breakpoints, native-disabled AppMenu items, reversed Dialog action order, stale visual-system debt, and motion that bypasses the reduced-motion contract.

The only global viewport projection is 767px for mobile behavior. The 768px/1199px shell projection belongs only to `AppShell` and `AppSidebar`; feature layout must use intrinsic sizing or container queries. Every guard rule has a focused failing fixture so governance changes remain narrow and reviewable.

Raw colors and legacy visual variables are rejected outside `tokens.css`; every component must consume the canonical semantic contract. The spacing scale intentionally has no 20px step.
