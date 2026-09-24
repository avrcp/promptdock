import type { ComponentHealth, HealthTone } from '@/contracts/common'
import type { DeviceState, GatewayState } from '@/contracts/device'

export type DeviceStatusTone = HealthTone

interface DeviceStatusVisual {
  tone: DeviceStatusTone
  label: string
}

const DEVICE_STATE_VISUALS: Record<DeviceState, DeviceStatusVisual> = {
  online: { tone: 'success', label: '在线' },
  offline: { tone: 'muted', label: '离线' },
  disabled: { tone: 'warning', label: '已禁用' },
  revoked: { tone: 'danger', label: '已撤销' },
  unknown: { tone: 'muted', label: '未配置' },
}

export function deviceStateVisual(state: DeviceState): DeviceStatusVisual {
  return DEVICE_STATE_VISUALS[state]
}

interface GatewayStatusVisual {
  tone: DeviceStatusTone
  label: string
}

const GATEWAY_VISUALS: Record<GatewayState, GatewayStatusVisual> = {
  connected: { tone: 'success', label: '已连接' },
  reconnecting: { tone: 'warning', label: '重连中' },
  offline: { tone: 'muted', label: '离线' },
  superseded: { tone: 'muted', label: '已被替代' },
  not_enabled: { tone: 'muted', label: '未启用' },
}

export function gatewayStateVisual(state: GatewayState): GatewayStatusVisual {
  return GATEWAY_VISUALS[state]
}

export function isDeviceActionable(item: { state: DeviceState }): boolean {
  return item.state !== 'revoked'
}

export function canDisable(item: { state: DeviceState }): boolean {
  return item.state === 'online' || item.state === 'offline' || item.state === 'unknown'
}

export function canEnable(item: { state: DeviceState }): boolean {
  return item.state === 'disabled'
}

export function shortDeviceId(id: string): string {
  if (id.length <= 10) return id
  return `${id.slice(0, 4)}…${id.slice(-4)}`
}

interface HealthToneVisual {
  tone: HealthTone
  label: string
}

const HEALTH_VISUALS: Record<ComponentHealth, HealthToneVisual> = {
  healthy: { tone: 'success', label: '正常' },
  degraded: { tone: 'warning', label: '不稳定' },
  failed: { tone: 'danger', label: '失败' },
  disabled: { tone: 'muted', label: '已关闭' },
  unknown: { tone: 'muted', label: '未配置' },
}

export function componentHealthVisual(health: ComponentHealth): HealthToneVisual {
  return HEALTH_VISUALS[health]
}
