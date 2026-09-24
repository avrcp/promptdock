import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs'
import { relative, resolve } from 'node:path'

import { describe, expect, it } from 'vitest'

const sourceRoot = resolve(process.cwd(), 'src')
const tokenFile = resolve(sourceRoot, 'design-system/tokens.css')
const motionFile = resolve(sourceRoot, 'design-system/motion.css')
const readmeFile = resolve(sourceRoot, 'design-system/README.md')
const guardFile = resolve(sourceRoot, 'design-system/design-token-guard.test.ts')
const appMenuFile = resolve(sourceRoot, 'components/AppMenu.vue')
const appDialogFile = resolve(sourceRoot, 'components/AppDialog.vue')
const toastComposableFile = resolve(sourceRoot, 'composables/useToast.ts')

const truePillRadiusAllowlist: Readonly<Record<string, readonly string[]>> = {
  'components/AppTabs.vue': ['999px'],
}

const legacyTokenPattern =
  /--(?:bg-|text-(?:primary|secondary|muted)|border-(?:default|strong|muted)|accent(?:-hover)?|focus-ring|status-|shadow-|radius-|font-(?:sans|mono|size-)|space-|shell-|table-row-height|button-height-|focus-target-min|transition-(?:fast|base))\b/g

const shadowOwners = new Set([
  'components/AppDialog.vue',
  'components/AppDrawer.vue',
  'components/AppMenu.vue',
  'components/CredentialReceiptDialog.vue',
])

const shellBreakpointOwners = new Set(['app/AppShell.vue', 'app/AppSidebar.vue'])

const requiredTokens = [
  '--pd-palette-accent',
  '--pd-container-workspace-bg',
  '--pd-text-default',
  '--pd-border-focus',
  '--pd-border-separator',
  '--pd-border-emphasis',
  '--pd-border-overlay',
  '--pd-font-interface',
  '--pd-font-code',
  '--pd-shell-content-padding',
  '--pd-control-height-touch',
  '--pd-icon-button-size-sm',
  '--pd-icon-button-size-md',
  '--pd-keycap-height',
  '--pd-badge-height',
  '--pd-empty-state-min-block-size',
  '--pd-drawer-max-width',
  '--pd-dialog-max-width',
  '--pd-status-chip-height',
  '--pd-ease-standard',
  '--pd-transition-reveal',
] as const

/**
 * Bare height values that must be expressed via a design token instead.
 * The regex targets only CSS `height:` declarations with these exact pixel
 * literals. It intentionally avoids matching padding, min-height, SVG
 * attributes, or unrelated numeric contexts.
 */
const bareHeightPattern = /(?<![-\w])height\s*:\s*(22|26|28|132)px\b/g

function sourceFiles(directory: string): string[] {
  return readdirSync(directory).flatMap((name) => {
    const path = resolve(directory, name)
    if (statSync(path).isDirectory()) return sourceFiles(path)
    return /\.(css|ts|vue)$/.test(name) ? [path] : []
  })
}

function consumeAllowance(allowlist: string[] | undefined, value: string): boolean {
  if (!allowlist) return false
  const index = allowlist.indexOf(value)
  if (index === -1) return false
  allowlist.splice(index, 1)
  return true
}

function findMissingTokenFailures(tokens: string): string[] {
  return requiredTokens
    .filter((token) => !new RegExp(`${token}\\s*:`).test(tokens))
    .map((token) => `tokens.css: missing ${token}`)
}

function styleContent(path: string, source: string): string {
  if (path.endsWith('.css')) return source
  if (!path.endsWith('.vue')) return ''
  return [...source.matchAll(/<style[^>]*>([\s\S]*?)<\/style>/gi)]
    .map((match) => match[1])
    .join('\n')
}

function findViewportBreakpointFailures(path: string, source: string): string[] {
  const failures: string[] = []
  const styles = styleContent(path, source)

  for (const media of styles.matchAll(/@media\s*([^{]+)[{]/gi)) {
    for (const width of media[1]?.matchAll(/(?:min|max)-width\s*:\s*(\d+)px\b/gi) ?? []) {
      const value = Number(width[1])
      const isGlobalMobileProjection = value === 767
      const isShellProjection = shellBreakpointOwners.has(path) && (value === 768 || value === 1199)
      if (!isGlobalMobileProjection && !isShellProjection) {
        failures.push(`${path}: unauthorized viewport breakpoint ${value}px`)
      }
    }
  }

  return failures
}

function findLegacyNumericFailures(path: string, source: string): string[] {
  const failures: string[] = []
  const styles = styleContent(path, source)
  const markup = path.endsWith('.vue') ? source : ''

  for (const match of markup.matchAll(/\bclass\s*=\s*(["'])(.*?)\1/gs)) {
    if (match[2]?.split(/\s+/).includes('numeric')) {
      failures.push(`${path}: legacy .numeric class`)
    }
  }

  if (/(?:^|[,{}]\s*)\.numeric(?=[\s,{:.#[])/m.test(styles)) {
    failures.push(`${path}: legacy .numeric selector`)
  }

  return failures
}

function findBareHeightFailures(path: string, source: string): string[] {
  return [...styleContent(path, source).matchAll(bareHeightPattern)].map(
    (match) => `${path}: bare height ${match[1]}px (use a design token)`,
  )
}

function hasNativeDisabledMenuItem(source: string): boolean {
  return [...source.matchAll(/<button\b[^>]*>/gi)].some((match) => {
    const tag = match[0]
    return (
      /\brole\s*=\s*["']menuitem["']/i.test(tag) &&
      /\s(?:(?::|v-bind:)disabled|disabled)(?=\s|=|\/?>)/i.test(tag)
    )
  })
}

function hasReversedDialogFooter(source: string): boolean {
  const styles = styleContent('components/AppDialog.vue', source)
  return /\.app-dialog__footer\s*\{[^}]*\bflex-direction\s*:\s*column-reverse\b/is.test(styles)
}

function findMotionContractFailures(tokens: string, motion: string): string[] {
  const failures: string[] = []

  if (!/--pd-ease-standard\s*:\s*cubic-bezier\(0\.2\s*,\s*0\s*,\s*0\s*,\s*1\)\s*;/i.test(tokens)) {
    failures.push('tokens.css: missing standard ease-out token')
  }

  for (const token of [
    '--pd-transition-fast',
    '--pd-transition-normal',
    '--pd-transition-reveal',
  ]) {
    const declaration = new RegExp(
      `${token}\\s*:\\s*\\d+ms\\s+var\\(--pd-ease-standard\\)\\s*;`,
      'i',
    )
    if (!declaration.test(tokens)) failures.push(`tokens.css: ${token} must use standard easing`)
  }

  const reducedTokens = tokens.match(
    /@media\s*\(prefers-reduced-motion\s*:\s*reduce\)\s*\{\s*:root\s*\{([\s\S]*?)\}\s*\}/i,
  )?.[1]
  for (const token of [
    '--pd-transition-fast',
    '--pd-transition-normal',
    '--pd-transition-reveal',
  ]) {
    const zeroDuration = new RegExp(`${token}\\s*:\\s*0ms\\s*;`, 'i')
    if (!reducedTokens || !zeroDuration.test(reducedTokens)) {
      failures.push(`tokens.css: reduced motion must zero ${token}`)
    }
  }

  const reducedMotion = motion.match(
    /@media\s*\(prefers-reduced-motion\s*:\s*reduce\)\s*\{([\s\S]*?)\}\s*\}/i,
  )?.[1]
  if (!reducedMotion || !/\.pd-spin\s*\{[^}]*\banimation\s*:\s*none\s*;/is.test(reducedMotion)) {
    failures.push('motion.css: reduced motion must disable continuous spinning')
  }

  const pressBlock = motion.match(
    /@media\s*\(prefers-reduced-motion\s*:\s*no-preference\)\s*\{([\s\S]*?)\}\s*\}/i,
  )
  if (!pressBlock || !/\btransform\s*:\s*translateY\(1px\)\s*;/i.test(pressBlock[1] ?? '')) {
    failures.push('motion.css: press feedback must be gated by no-preference')
  }
  const motionWithoutPressGate = pressBlock ? motion.replace(pressBlock[0], '') : motion
  if (/\btransform\s*:\s*translateY\(1px\)\s*;/i.test(motionWithoutPressGate)) {
    failures.push('motion.css: press feedback exists outside no-preference')
  }

  return failures
}

function findToastDebtFailures(
  tokens: string,
  motion: string,
  readme: string,
  hasComposable: boolean,
): string[] {
  const failures: string[] = []
  if (hasComposable) failures.push('composables/useToast.ts: unused Toast composable')
  if (/--pd-z-toast\b/i.test(tokens)) failures.push('tokens.css: unused Toast z-index token')
  if (/\bpd-toast(?:-in|-out|-enter-active|-leave-active)?\b/i.test(motion)) {
    failures.push('motion.css: unused Toast motion primitive')
  }
  if (/\btoast\b/i.test(readme)) failures.push('design-system/README.md: stale Toast documentation')
  return failures
}

describe('design guard rule fixtures', () => {
  it('rejects a missing semantic role token declaration', () => {
    expect(findMissingTokenFailures(':root { --pd-palette-accent: red; }')).toContain(
      'tokens.css: missing --pd-keycap-height',
    )
  })

  it('rejects feature viewport magic breakpoints without confusing intrinsic layouts', () => {
    const fixture = '<style>.panel { padding-inline: 999px } @media (max-width: 999px) {}</style>'
    expect(findViewportBreakpointFailures('features/example/ExamplePage.vue', fixture)).toEqual([
      'features/example/ExamplePage.vue: unauthorized viewport breakpoint 999px',
    ])
    expect(
      findViewportBreakpointFailures(
        'features/example/ExamplePage.vue',
        '<style>@media (max-width: 1199px) {}</style>',
      ),
    ).toEqual(['features/example/ExamplePage.vue: unauthorized viewport breakpoint 1199px'])
    expect(
      findViewportBreakpointFailures(
        'features/example/ExamplePage.vue',
        '<style>@container (max-width: 44rem) {} @media (max-width: 767px) {}</style>',
      ),
    ).toEqual([])
    expect(
      findViewportBreakpointFailures(
        'app/AppSidebar.vue',
        '<style>@media (max-width: 1199px) and (min-width: 768px) {}</style>',
      ),
    ).toEqual([])
  })

  it('rejects only an exact legacy numeric class or selector', () => {
    const fixture =
      '<template><span class="mono numeric">1</span></template><style>.numeric {}</style>'
    expect(findLegacyNumericFailures('features/example/ExamplePage.vue', fixture)).toHaveLength(2)
    expect(
      findLegacyNumericFailures(
        'features/example/ExamplePage.vue',
        '<template><input inputmode="numeric" class="tabular" /></template>',
      ),
    ).toEqual([])
  })

  it('rejects bare role heights without matching other 28px values', () => {
    expect(
      findBareHeightFailures(
        'components/Example.vue',
        '<style>.control { height: 28px; padding-inline: 28px; min-height: 28px; }</style>',
      ),
    ).toEqual(['components/Example.vue: bare height 28px (use a design token)'])
  })

  it('rejects native disabled only on AppMenu menu items', () => {
    expect(
      hasNativeDisabledMenuItem(
        '<button role="menuitem" :disabled="item.disabled">Unavailable</button>',
      ),
    ).toBe(true)
    expect(
      hasNativeDisabledMenuItem(
        '<button role="menuitem" :aria-disabled="item.disabled">Unavailable</button>',
      ),
    ).toBe(false)
  })

  it('rejects column-reverse only on the Dialog action footer', () => {
    expect(
      hasReversedDialogFooter(
        '<style>.app-dialog__footer { flex-direction: column-reverse; }</style>',
      ),
    ).toBe(true)
    expect(
      hasReversedDialogFooter(
        '<style>.unrelated-history { flex-direction: column-reverse; }</style>',
      ),
    ).toBe(false)
  })

  it('rejects missing motion tokens and reduced-motion safeguards', () => {
    expect(
      findMotionContractFailures(
        ':root { --pd-transition-fast: 120ms ease; }',
        '.app-button:active { transform: translateY(1px); }',
      ).length,
    ).toBeGreaterThan(0)
  })

  it('rejects every stale Toast layer', () => {
    expect(
      findToastDebtFailures(
        ':root { --pd-z-toast: 70; }',
        '.pd-toast-enter-active {}',
        'Toast primitives are available.',
        true,
      ),
    ).toHaveLength(4)
  })
})

describe('design system guard', () => {
  it('keeps the canonical token contract complete', () => {
    const tokens = readFileSync(tokenFile, 'utf8')
    expect(findMissingTokenFailures(tokens)).toEqual([])
    expect(tokens).toContain("'Noto Sans SC'")
    expect(tokens).not.toContain('--space-5')
    expect(tokens).not.toMatch(/--pd-space-5\s*:/)
  })

  it('rejects deleted border tokens', () => {
    const tokens = readFileSync(tokenFile, 'utf8')
    expect(tokens).not.toMatch(/--pd-border-muted\s*:/)
    expect(tokens).not.toMatch(/--pd-border-default\s*:/)
    expect(tokens).not.toMatch(/--pd-border-strong\s*:/)
  })

  it('rejects visual debt outside the documented migration boundary', () => {
    const failures: string[] = []
    const radiusAllowances = Object.fromEntries(
      Object.entries(truePillRadiusAllowlist).map(([file, values]) => [file, [...values]]),
    )

    for (const file of sourceFiles(sourceRoot)) {
      if (file === tokenFile || file === guardFile) continue
      const path = relative(sourceRoot, file).replace(/\\/g, '/')
      const source = readFileSync(file, 'utf8')

      const rawColors =
        source.match(/#[0-9a-fA-F]{3,8}(?![0-9a-fA-F\w])|\b(?:rgb|hsl)a?\([^)]*\)/g) ?? []
      for (const color of rawColors) failures.push(`${path}: raw color ${color}`)

      for (const token of source.match(legacyTokenPattern) ?? [])
        failures.push(`${path}: legacy token ${token}`)

      if (/transition\s*:\s*all\b/i.test(source)) failures.push(`${path}: transition: all`)

      for (const match of source.matchAll(/border-radius\s*:\s*(\d+)px/gi)) {
        const radius = `${match[1]}px`
        if (Number(match[1]) > 8 && !consumeAllowance(radiusAllowances[path], radius)) {
          failures.push(`${path}: oversized radius ${radius}`)
        }
      }

      if (/box-shadow\s*:/i.test(source) && !shadowOwners.has(path)) {
        failures.push(`${path}: unauthorized box-shadow owner`)
      }

      failures.push(...findBareHeightFailures(path, source))
      failures.push(...findViewportBreakpointFailures(path, source))
      failures.push(...findLegacyNumericFailures(path, source))
    }

    expect(failures).toEqual([])
  })

  it('keeps AppMenu unavailable actions discoverable and Dialog actions in DOM order', () => {
    expect(hasNativeDisabledMenuItem(readFileSync(appMenuFile, 'utf8'))).toBe(false)
    expect(hasReversedDialogFooter(readFileSync(appDialogFile, 'utf8'))).toBe(false)
  })

  it('keeps functional motion tokenized and safe for reduced-motion users', () => {
    expect(
      findMotionContractFailures(readFileSync(tokenFile, 'utf8'), readFileSync(motionFile, 'utf8')),
    ).toEqual([])
  })

  it('does not retain an unused Toast implementation or documentation', () => {
    expect(
      findToastDebtFailures(
        readFileSync(tokenFile, 'utf8'),
        readFileSync(motionFile, 'utf8'),
        readFileSync(readmeFile, 'utf8'),
        existsSync(toastComposableFile),
      ),
    ).toEqual([])
  })

  it('restricts --pd-text-hint to placeholder, disabled, or decorative contexts', () => {
    const failures: string[] = []
    const hintAllowlist: Readonly<Record<string, string>> = {
      'features/overview/OverviewPage.vue': 'decorative dot',
    }

    for (const file of sourceFiles(sourceRoot)) {
      if (file === tokenFile || file === guardFile) continue
      const path = relative(sourceRoot, file).replace(/\\/g, '/')
      const source = readFileSync(file, 'utf8')

      if (source.includes('--pd-text-hint') && !(path in hintAllowlist)) {
        failures.push(`${path}: unauthorized --pd-text-hint usage`)
      }
    }

    expect(failures).toEqual([])
  })
})
