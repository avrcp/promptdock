import type { HealthTone } from '@/contracts/common'
import type {
  DeliveryKind,
  DeliveryOrigin,
  DeliveryState,
  InboundCommand,
  InboundState,
} from '@/contracts/queue'

export type QueueStatusTone = HealthTone

interface QueueStatusVisual {
  tone: QueueStatusTone
  label: string
}

const DELIVERY_STATE_VISUALS: Record<DeliveryState, QueueStatusVisual> = {
  queued: { tone: 'info', label: '排队中' },
  sending: { tone: 'info', label: '发送中' },
  retrying: { tone: 'warning', label: '重试中' },
  blocked: { tone: 'warning', label: '已阻塞' },
  accepted: { tone: 'success', label: '已接收' },
  failed: { tone: 'danger', label: '失败' },
  cancelled: { tone: 'muted', label: '已取消' },
  expired: { tone: 'muted', label: '已过期' },
}

export function deliveryStateVisual(state: DeliveryState): QueueStatusVisual {
  return DELIVERY_STATE_VISUALS[state]
}

const INBOUND_STATE_VISUALS: Record<InboundState, QueueStatusVisual> = {
  received: { tone: 'info', label: '已接收' },
  dispatching: { tone: 'info', label: '分发中' },
  waiting_gateway: { tone: 'warning', label: '等待 Gateway' },
  reply_queued: { tone: 'info', label: '回复排队' },
  expired: { tone: 'muted', label: '已过期' },
  dead_letter: { tone: 'danger', label: '死信' },
}

export function inboundStateVisual(state: InboundState): QueueStatusVisual {
  return INBOUND_STATE_VISUALS[state]
}

export function replyStateVisual(state: DeliveryState): QueueStatusVisual {
  return DELIVERY_STATE_VISUALS[state]
}

const DELIVERY_KIND_VISUALS: Record<DeliveryKind, string> = {
  run_event: '运行事件',
  test: '测试',
  interactive_reply: '交互回复',
  activation: '激活',
}

export function deliveryKindVisual(kind: DeliveryKind): string {
  return DELIVERY_KIND_VISUALS[kind]
}

const DELIVERY_ORIGIN_VISUALS: Record<DeliveryOrigin, string> = {
  device: '设备',
  system: '系统',
  admin: 'Admin 控制台',
}

export function deliveryOriginVisual(origin: DeliveryOrigin): string {
  return DELIVERY_ORIGIN_VISUALS[origin]
}

export function deliveryPriorityVisual(priority: number): string {
  if (priority >= 200) return `紧急 (${priority})`
  if (priority >= 100) return `高 (${priority})`
  if (priority >= 1) return `普通 (${priority})`
  return '最低 (0)'
}

export function deliveryPriorityLabel(priority: number): string {
  if (priority >= 200) return '紧急'
  if (priority >= 100) return '高'
  if (priority >= 1) return '普通'
  return '最低'
}

const INBOUND_COMMAND_VISUALS: Record<InboundCommand, string> = {
  help: '帮助',
  list_devices: '设备',
  list_jobs: '任务',
  list_recent: '最近',
  list_failed: '失败',
  next_page: '下一页',
  get_status: '查看状态',
  get_detail: '查看详情',
  get_tree: '查看子运行',
  unknown: '未知命令',
}

export function inboundCommandVisual(command: InboundCommand): string {
  return INBOUND_COMMAND_VISUALS[command]
}
