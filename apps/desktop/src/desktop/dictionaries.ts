import type { StatusTone } from '../components/StatusChip.vue'

export type Delivery = {
  id: string
  status: string
  remoteStatus: string | null
  lastErrorCode?: string | null
  createdAt: number
  payload: { title: string }
  result?: ResultPublication | null
}

export type ResultPublication = {
  resultId: string
  sourceHash: string
  pageState: 'available' | 'revoked' | 'expired' | 'content_unavailable' | null
  pageExpiresAt: number | null
  notificationId: string | null
  notificationStatus: string | null
}

export function resultPageLabel(result: ResultPublication): string {
  switch (result.pageState) {
    case 'available':
      return result.pageExpiresAt
        ? `可查看至 ${formatDeliveryTime(result.pageExpiresAt)}`
        : '已保存'
    case 'revoked':
      return '已撤销'
    case 'expired':
      return '已过期'
    case 'content_unavailable':
      return '正文不可用'
    default:
      return '等待服务器接管'
  }
}

/** 本机状态与服务器状态的中文标签；未知状态如实标注，不臆造。 */
export const hookStateLabels: Readonly<Record<string, string>> = {
  not_configured: '未配置',
  configured_trust_unverified: '已配置，信任尚未核验',
  configured_trusted: '已配置，已信任',
  configured_trust_modified: '已配置，信任已失效',
  configured_disabled: '已配置，已停用',
  needs_repair: '需修复',
  configuration_error: '配置检查失败',
}

const deliveryStates = {
  user_held: { label: '推送已暂缓，记录仍保存在本机', tone: 'neutral' },
  relay_accepted: { label: 'Relay 已接管', tone: 'success' },
  pending: { label: '等待提交', tone: 'warning' },
  sending: { label: '正在提交', tone: 'warning' },
  blocked: { label: '等待恢复', tone: 'warning' },
  delivered: { label: 'Relay 已接管', tone: 'success' },
  pending_channel: { label: '服务器排队中', tone: 'warning' },
  sending_channel: { label: '正在发送至微信', tone: 'warning' },
  retry_wait: { label: '等待重试', tone: 'warning' },
  blocked_activation: { label: '等待微信激活', tone: 'warning' },
  blocked_reconnect: { label: '等待微信重新连接', tone: 'warning' },
  provider_accepted: { label: '微信接口已接收（手机显示待核验）', tone: 'success' },
  dead_letter: { label: '投递失败，需处理', tone: 'danger' },
  partial_failed: { label: '部分投递失败，需处理', tone: 'danger' },
  cancelled: { label: '已取消', tone: 'danger' },
  expired: { label: '已过期', tone: 'danger' },
  blocked_target_changed: { label: '微信账号已变化，旧正文已阻止发送', tone: 'danger' },
  delivery_unknown: { label: '投递结果未知，需核验', tone: 'danger' },
} as const satisfies Readonly<Record<string, { label: string; tone: StatusTone }>>

export const deliveryStateLabels: Readonly<Record<string, string>> = Object.fromEntries(
  Object.entries(deliveryStates).map(([state, { label }]) => [state, label]),
)

export function deliveryLabel(state: string | null | undefined): string {
  if (!state) return '状态待确认'
  return deliveryStates[state as keyof typeof deliveryStates]?.label ?? '状态待确认'
}

export function deliveryTone(state: string | null | undefined): StatusTone {
  return deliveryStates[state as keyof typeof deliveryStates]?.tone ?? 'warning'
}

export function formatDeliveryTime(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return '时间未知'
  const milliseconds = value > 10_000_000_000 ? value : value * 1000
  return new Intl.DateTimeFormat('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  }).format(new Date(milliseconds))
}

export function deliveryIso(value: number): string | undefined {
  if (!Number.isFinite(value) || value <= 0) return undefined
  const milliseconds = value > 10_000_000_000 ? value : value * 1000
  return new Date(milliseconds).toISOString()
}
