import type { AdminOverview } from '@/contracts/overview'
import type { ChannelEventItem, ChannelEventQuery, WechatAdminStatus } from '@/contracts/wechat'
import type { DeviceDetail, DeviceListItem, DeviceListQuery } from '@/contracts/device'
import type {
  DeliveryListItem,
  DeliveryListQuery,
  InboundCommandListItem,
  InboundCommandListQuery,
  InteractiveReplyListItem,
  InteractiveReplyListQuery,
} from '@/contracts/queue'
import type { SystemSnapshot } from '@/contracts/system'
import type { ResultListItem, ResultListQuery } from '@/contracts/result'
import type { Page, RequestOptions } from '@/contracts/common'
import type { AdminMeta } from '@promptdock/relay-admin-api-generated'

export interface AdminReadRepository {
  getMeta?: (options?: RequestOptions) => Promise<AdminMeta>
  getOverview(options?: RequestOptions): Promise<AdminOverview>
  listDevices(query: DeviceListQuery, options?: RequestOptions): Promise<Page<DeviceListItem>>
  getDevice(deviceId: string, options?: RequestOptions): Promise<DeviceDetail>
  getWechatStatus(options?: RequestOptions): Promise<WechatAdminStatus>
  listChannelEvents(
    query: ChannelEventQuery,
    options?: RequestOptions,
  ): Promise<Page<ChannelEventItem>>
  listDeliveries(
    query: DeliveryListQuery,
    options?: RequestOptions,
  ): Promise<Page<DeliveryListItem>>
  listInteractiveReplies(
    query: InteractiveReplyListQuery,
    options?: RequestOptions,
  ): Promise<Page<InteractiveReplyListItem>>
  listInboundCommands(
    query: InboundCommandListQuery,
    options?: RequestOptions,
  ): Promise<Page<InboundCommandListItem>>
  listResults(query: ResultListQuery, options?: RequestOptions): Promise<Page<ResultListItem>>
  getSystemSnapshot(options?: RequestOptions): Promise<SystemSnapshot>
}
