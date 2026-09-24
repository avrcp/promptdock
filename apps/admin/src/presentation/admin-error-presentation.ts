import type { AdminError } from '@/contracts/error'

/**
 * A user-facing recovery action.  Each kind maps to a specific affordance
 * the calling page must render (a retry button, a navigation link, etc).
 * Centralising this prevents pages from accidentally treating
 * `error.message` as the headline copy and forgetting the next step.
 */
export type AdminErrorAction =
  | { kind: 'retry'; label: string }
  | { kind: 'navigate'; label: string; to: string }
  | { kind: 'copy'; label: string; value: string }
  | { kind: 'none' }

export interface AdminErrorPresentation {
  /** Headline copy the operator sees first.  Short, declarative. */
  title: string
  /** Secondary explanation.  May be empty for already self-explanatory titles. */
  description: string
  /** Recovery action, if any. */
  action: AdminErrorAction
  /**
   * Whether the action verb is retryable on a delay.  Pages that show a
   * countdown should consult this rather than re-derive from the
   * underlying error.
   */
  retryable: boolean
  /** Optional request id; if present, copyable via the copy action. */
  requestId: string | null
}

const RETRY_LABEL = '重试'
const COPY_REQUEST_ID_LABEL = '复制 Request ID'

/**
 * Map an `AdminError` to user-facing copy + a recovery action.  Pages
 * SHOULD consume this rather than showing the raw error object.
 */
export function presentAdminError(error: AdminError): AdminErrorPresentation {
  const requestId = error.requestId
  const copyAction: AdminErrorAction | null = requestId
    ? { kind: 'copy', label: COPY_REQUEST_ID_LABEL, value: requestId }
    : null
  switch (error.code) {
    case 'RELAY_UNAVAILABLE':
      return {
        title: '无法连接 Relay',
        description: '当前显示的可能已是陈旧数据；可稍后重试。',
        action: { kind: 'retry', label: RETRY_LABEL },
        retryable: true,
        requestId,
      }
    case 'WECHAT_RECONNECT_REQUIRED':
      return {
        title: '微信通道需要重新登录',
        description: '请前往微信通道页扫码登录。',
        action: { kind: 'navigate', label: '前往微信通道', to: '/wechat' },
        retryable: false,
        requestId,
      }
    case 'DATABASE_DEGRADED':
      return {
        title: '数据库处于降级状态',
        description: error.message,
        action: { kind: 'retry', label: RETRY_LABEL },
        retryable: true,
        requestId,
      }
    case 'GATEWAY_TIMEOUT':
      return {
        title: 'Gateway 通信超时',
        description: error.message,
        action: { kind: 'retry', label: RETRY_LABEL },
        retryable: true,
        requestId,
      }
    case 'OUTBOX_BACKLOG':
      return {
        title: '投递队列积压',
        description: error.message,
        action: { kind: 'navigate', label: '查看队列', to: '/queue' },
        retryable: false,
        requestId,
      }
    case 'RATE_LIMITED':
      return {
        title: '操作过于频繁',
        description: '请稍后重试。',
        action: copyAction ?? { kind: 'none' },
        retryable: error.retryable,
        requestId,
      }
    case 'DEVICE_NOT_FOUND':
      return {
        title: '设备不存在',
        description: '该设备可能已被撤销或属于另一个数据源。',
        action: { kind: 'navigate', label: '返回设备列表', to: '/devices' },
        retryable: false,
        requestId,
      }
    case 'DEVICE_REVOKED':
      return {
        title: '设备已撤销',
        description: '此设备无法恢复，请新建设备并重新配置凭证。',
        action: { kind: 'navigate', label: '前往新建设备', to: '/devices' },
        retryable: false,
        requestId,
      }
    case 'WECHAT_LOGIN_GRANT_REJECTED':
    case 'WECHAT_LOGIN_NOT_FOUND':
    case 'WECHAT_LOGIN_ACCESS_REVOKED':
    case 'WECHAT_LOGIN_CONFLICT':
    case 'WECHAT_LOGIN_RATE_LIMITED':
      return {
        title: '微信登录授权失败',
        description: error.message,
        action: { kind: 'navigate', label: '前往微信通道', to: '/wechat' },
        retryable: false,
        requestId,
      }
    case 'WECHAT_UNAVAILABLE':
      return {
        title: '微信服务不可用',
        description: error.message,
        action: { kind: 'retry', label: RETRY_LABEL },
        retryable: true,
        requestId,
      }
    case 'CURSOR_INVALID':
    case 'CURSOR_EXPIRED':
      return {
        title: '列表已更新',
        description: '当前分页位置已失效，请从第一页重新加载。',
        action: { kind: 'retry', label: RETRY_LABEL },
        retryable: true,
        requestId,
      }
    case 'INVALID_INPUT':
      return {
        title: '输入内容不符合要求',
        description: error.message,
        action: { kind: 'none' },
        retryable: false,
        requestId,
      }
    case 'INTERNAL':
    default:
      return {
        title: '操作失败',
        description: error.message,
        action: copyAction ?? { kind: 'retry', label: RETRY_LABEL },
        retryable: error.retryable,
        requestId,
      }
  }
}
