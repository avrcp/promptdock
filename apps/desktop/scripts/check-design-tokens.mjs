#!/usr/bin/env node
/**
 * 设计令牌门禁（SSOT 守卫）
 *
 * 规则见 docs/desktop/design-tokens.md：
 *   1. src/design-system/tokens.css 是唯一允许出现裸 px / hex 的文件。
 *   2. 未被消费的令牌一律删除，不做「以防万一」的预留。
 *
 * 检查项：
 *   - 死令牌：定义了但全项目零引用
 *   - 未定义令牌引用：任何 var(--token) 都必须由 tokens.css 或同一源码集定义
 *   - 裸值：组件 / 视图 / 样式中出现裸 hex 或 color-mix()
 *   - 硬编码尺寸：样式中出现未走令牌的 px 数值（排除 0/1px/百分比/媒体查询）
 *
 * 退出码：发现问题为 1，否则 0。
 */

import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join, relative } from 'node:path'

const ROOT = process.cwd()
const SRC = join(ROOT, 'src')
const TOKENS_FILE = 'src/design-system/tokens.css'

function walk(dir) {
  return readdirSync(dir).flatMap((entry) => {
    const full = join(dir, entry)
    return statSync(full).isDirectory() ? walk(full) : [full]
  })
}

const files = walk(SRC).filter((f) => /\.(vue|ts|css)$/.test(f))
const docs = files.map((file) => ({
  path: relative(ROOT, file).replace(/\\/g, '/'),
  lines: readFileSync(file, 'utf8').split('\n'),
}))

/** 1. 死令牌 */
const definitions = new Map()
for (const { path, lines } of docs) {
  if (!path.endsWith('.css')) continue
  lines.forEach((line, index) => {
    const m = line.match(/^\s*(--[a-z0-9-]+)\s*:/)
    if (m && !definitions.has(m[1])) definitions.set(m[1], { path, index })
  })
}

const deadTokens = []
for (const [token, def] of definitions) {
  const consumed = docs.some(({ path, lines }) =>
    lines.some((line, index) => {
      if (path === def.path && index === def.index) return false
      return line.includes(`var(${token})`)
    }),
  )
  if (!consumed) deadTokens.push(`${token}  (定义于 ${def.path}:${def.index + 1})`)
}

/** 1b. 未定义令牌引用：CSS 自定义属性静默回退会让尺寸与颜色门禁失效。 */
const undefinedTokenUses = []
for (const { path, lines } of docs) {
  lines.forEach((line, index) => {
    for (const match of line.matchAll(/var\((--[a-z0-9-]+)/g)) {
      if (!definitions.has(match[1])) {
        undefinedTokenUses.push(`${match[1]}  (引用于 ${path}:${index + 1})`)
      }
    }
  })
}

/** 2 & 3. 裸值（tokens.css 之外） */
const bareValues = []
const PX_ALLOWED = new Set(['0', '1px', '-1px', '100%', '50%'])
for (const { path, lines } of docs) {
  if (path === TOKENS_FILE) continue
  lines.forEach((line, index) => {
    let code = line.split('/*')[0]
    if (code.includes('@media')) return // 媒体查询无法插值自定义属性
    if (/#[0-9a-fA-F]{3,8}\b/.test(code) || code.includes('color-mix(')) {
      bareValues.push(`${path}:${index + 1}  裸色值或 color-mix: ${line.trim()}`)
      return
    }
    // 先剥掉所有 var()，否则「9px var(--x)」这类混排会漏检
    code = code.replace(/var\([^)]*\)/g, '')
    const px = code.match(/(-?\d+(?:\.\d+)?)px/g)
    const offenders = (px ?? []).filter((value) => !PX_ALLOWED.has(value))
    if (offenders.length > 0) {
      bareValues.push(
        `${path}:${index + 1}  未走令牌的尺寸 ${offenders.join(', ')}: ${line.trim()}`,
      )
    }
  })
}

const report = (title, items) => {
  console.log(`\n${title}（${items.length}）`)
  if (items.length === 0) {
    console.log('  ✔ 无')
    return
  }
  items.forEach((item) => console.log(`  ✘ ${item}`))
}

report('未被消费的令牌', deadTokens)
report('未定义的令牌引用', undefinedTokenUses)
report('组件 / 视图中的裸值', bareValues)

const failed = deadTokens.length + undefinedTokenUses.length + bareValues.length > 0
console.log(failed ? '\nFAIL：设计令牌门禁未通过\n' : '\nPASS：设计令牌门禁通过\n')
process.exit(failed ? 1 : 0)
