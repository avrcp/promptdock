
const trusts = [
  'not_applicable',
  'unknown',
  'no_matching_record',
  'matching_record',
  'definition_changed',
  'explicitly_disabled',
] as const
const states = [
  'not_started',
  'waiting_submit',
  'submit_seen',
  'paired',
  'timed_out',
  'invalidated',
  'inconclusive',
] as const
export type HookHealth = {
  runtimeEpoch: string
  revision: number
  observedAt: number
  fresh: boolean
  observationEnabled: boolean
  hookSourcePath: string | null
  userStateSourcePath: string | null
  sourceResolution: 'confirmed' | 'candidate' | 'unknown'
  compatibility: 'validated' | 'unverified' | 'unsupported'
  hostBuild: string | null
  installation: 'absent' | 'current' | 'needs_repair' | 'checking' | 'error'
  registrationId: string | null
  definitionFingerprint: string | null
  diagnosticCode: string | null
  handlers: {
    event: string
    required: boolean
    configured: boolean
    trust: (typeof trusts)[number]
    evidenceSource: string
    lastErrorCode: string | null
    lastObservedAt: number | null
    observedRegistrationId: string | null
    observationCurrent: boolean
  }[]
  verification: {
    state: (typeof states)[number]
    verificationId: string | null
    validatedAt: number | null
    validatedDefinitionFingerprint: string | null
    hostAttribution: string
    instruction: string | null
  }
}
export function parseHookHealth(value: unknown): HookHealth | null {
  if (!value || typeof value !== 'object') return null
  const v = value as HookHealth
  const nullableText = (s: unknown) => s === null || typeof s === 'string'
  const nullableTime = (n: unknown) => n === null || (Number.isSafeInteger(n) && Number(n) >= 0)
  if (
    typeof v.runtimeEpoch !== 'string' ||
    !Number.isSafeInteger(v.revision) ||
    v.revision < 0 ||
    typeof v.fresh !== 'boolean' ||
    typeof v.observationEnabled !== 'boolean' ||
    !Number.isSafeInteger(v.observedAt) ||
    ![
      v.hookSourcePath,
      v.userStateSourcePath,
      v.hostBuild,
      v.registrationId,
      v.definitionFingerprint,
      v.diagnosticCode,
    ].every(nullableText)
  )
    return null
  if (
    !['absent', 'current', 'needs_repair', 'checking', 'error'].includes(v.installation) ||
    !['confirmed', 'candidate', 'unknown'].includes(v.sourceResolution) ||
    !['validated', 'unverified', 'unsupported'].includes(v.compatibility)
  )
    return null
  if (
    !Array.isArray(v.handlers) ||
    !v.handlers.every(
      (h) =>
        h &&
        ['UserPromptSubmit', 'Stop', 'PermissionRequest'].includes(h.event) &&
        typeof h.required === 'boolean' &&
        typeof h.configured === 'boolean' &&
        typeof h.observationCurrent === 'boolean' &&
        nullableTime(h.lastObservedAt) &&
        nullableText(h.observedRegistrationId) &&
        nullableText(h.lastErrorCode) &&
        trusts.includes(h.trust),
    )
  )
    return null
  if (!v.verification || !states.includes(v.verification.state)) return null
  if (
    !nullableTime(v.verification.validatedAt) ||
    ![
      v.verification.verificationId,
      v.verification.validatedDefinitionFingerprint,
      v.verification.instruction,
    ].every(nullableText)
  )
    return null
  return v
}
export const trustLabels: Record<HookHealth['handlers'][number]['trust'], string> = {
  not_applicable: '不适用',
  unknown: '暂不可确认',
  no_matching_record: '未找到匹配记录',
  matching_record: '匹配持久信任记录',
  definition_changed: '定义已变化，请重新审阅',
  explicitly_disabled: '已在宿主禁用',
}
export const verificationLabels: Record<HookHealth['verification']['state'], string> = {
  not_started: '尚未验证实际触发',
  waiting_submit: '等待匹配的验证提交',
  submit_seen: '已收到验证提交，等待同轮结束',
  paired: '已关联本次提交和结束事件',
  timed_out: '等待已结束，未收到完整验证事件',
  invalidated: '定义、注册或构建已变化，请重新验证',
  inconclusive: '证据不足，暂不能完成验证',
}
