import type { AdminOverview } from '@/contracts/overview'
import type { DeviceDetail, DeviceListItem, DeviceListQuery, DeviceState } from '@/contracts/device'
import type { ChannelEventItem, ChannelEventQuery, WechatAdminStatus } from '@/contracts/wechat'
import type {
  DeliveryListItem,
  DeliveryListQuery,
  InboundCommandListItem,
  InboundCommandListQuery,
  InteractiveReplyListItem,
  InteractiveReplyListQuery,
} from '@/contracts/queue'
import type { SystemSnapshot } from '@/contracts/system'
import type { CurrentAlert, Page, RequestOptions } from '@/contracts/common'
import type { ScenarioId } from './scenario-ids'

export interface ScenarioSlice {
  readonly overview: AdminOverview
  readonly wechat: WechatAdminStatus
  readonly devices: DeviceListItem[]
  readonly deviceDetails: Record<string, DeviceDetail>
  readonly channelEvents: ChannelEventItem[]
  readonly deliveries: DeliveryListItem[]
  readonly replies: InteractiveReplyListItem[]
  readonly inbound: InboundCommandListItem[]
  readonly system: SystemSnapshot
  readonly alerts: CurrentAlert[]
}

export interface ScenarioViewFailure {
  readonly overview: 'failed' | 'degraded' | 'ok'
  readonly wechat: 'failed' | 'degraded' | 'ok'
  readonly devices: 'failed' | 'degraded' | 'ok'
  readonly queue: 'failed' | 'degraded' | 'ok'
  readonly system: 'failed' | 'degraded' | 'ok'
}

export interface ScenarioBehavior {
  readonly failureProfile?: Partial<ScenarioViewFailure>
  readonly delayProfile?: 'normal' | 'slow'
  readonly failingSections?: ReadonlyArray<keyof ScenarioViewFailure>
}

export interface ScenarioFixture {
  readonly id: ScenarioId
  readonly label: string
  readonly description: string
  readonly data: ScenarioSlice
  readonly behavior: ScenarioBehavior
}

export type { DeviceListQuery, DeliveryListQuery, ChannelEventQuery }
export type {
  InteractiveReplyListQuery,
  InboundCommandListQuery,
  RequestOptions,
  Page,
  DeviceState,
}
