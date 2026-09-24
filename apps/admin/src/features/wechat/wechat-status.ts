import type { HealthTone } from '@/contracts/common'
import type { WechatState, WechatLoginState } from '@/contracts/wechat'

export type WechatStatusTone = HealthTone

export interface WechatStatusVisual {
  tone: WechatStatusTone
  label: string
}

const WECHAT_STATE_VISUALS: Record<WechatState, WechatStatusVisual> = {
  disconnected: { tone: 'muted', label: '未连接' },
  connected_awaiting_activation: { tone: 'warning', label: '已登录，等待激活' },
  ready: { tone: 'success', label: '已就绪' },
  degraded: { tone: 'warning', label: '连接不稳定' },
  needs_reconnect: { tone: 'warning', label: '需要重新登录' },
  credentials_unreadable: { tone: 'danger', label: '凭据不可读取' },
}

export function wechatStateVisual(state: WechatState): WechatStatusVisual {
  return WECHAT_STATE_VISUALS[state]
}

export interface LoginStateVisual {
  tone: WechatStatusTone
  label: string
}

const LOGIN_STATE_VISUALS: Record<WechatLoginState, LoginStateVisual> = {
  fetching_qr: { tone: 'info', label: '获取二维码' },
  waiting_scan: { tone: 'info', label: '请扫码' },
  scanned: { tone: 'info', label: '已扫码' },
  verify_code_required: { tone: 'warning', label: '需要验证码' },
  refreshing_qr: { tone: 'info', label: '刷新二维码' },
  confirmed: { tone: 'success', label: '登录成功' },
  already_connected: { tone: 'success', label: '已连接' },
  expired: { tone: 'warning', label: '已过期' },
  cancelled: { tone: 'muted', label: '已取消' },
  failed: { tone: 'danger', label: '失败' },
}

export function loginStateVisual(state: WechatLoginState): LoginStateVisual {
  return LOGIN_STATE_VISUALS[state]
}
