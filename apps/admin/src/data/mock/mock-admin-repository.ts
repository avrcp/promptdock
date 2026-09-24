import type { AdminReadRepository } from '../admin-read-repository'
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
import { adminErrorSchema, AdminRepositoryError, type AdminError } from '@/contracts/error'

import { DEFAULT_MOCK_DELAY, SLOW_MOCK_DELAY, abortableDelay } from './mock-delay'
import type { MockClock } from './mock-clock'
import type { MockDeviceMutationState } from './mock-command-repository'
import { getScenarioById, type ScenarioFixture, type ScenarioId } from './scenarios'

export interface MockAdminReadRepositoryOptions {
  clock: MockClock
  initialScenario?: ScenarioId
  getScenarioId: () => ScenarioId
  requestIdFactory?: () => string
  delaySource?: () => number
  zeroDelay?: boolean
  failureOverride?: () => Partial<{
    overview: 'failed' | 'degraded' | 'ok'
    wechat: 'failed' | 'degraded' | 'ok'
    devices: 'failed' | 'degraded' | 'ok'
    queue: 'failed' | 'degraded' | 'ok'
    system: 'failed' | 'degraded' | 'ok'
  }>
  mutationState?: MockDeviceMutationState
}

function paginate<T>(
  items: T[],
  limit: number,
  cursor: string | null,
): { items: T[]; nextCursor: string | null; total: number | null } {
  const startIndex = cursor ? Math.max(0, Number.parseInt(cursor, 10) || 0) : 0
  const endIndex = Math.min(items.length, startIndex + limit)
  const pageItems = items.slice(startIndex, endIndex)
  const nextCursor = endIndex < items.length ? String(endIndex) : null
  return {
    items: pageItems,
    nextCursor,
    total: items.length,
  }
}

function buildAdminError(
  code: AdminError['code'],
  message: string,
  retryable: boolean,
  requestId: string | null,
): AdminError {
  return adminErrorSchema.parse({ code, message, retryable, requestId })
}

function sectionFailureError(
  section: 'overview' | 'wechat' | 'devices' | 'queue' | 'system',
  requestId: string | null,
): AdminError {
  switch (section) {
    case 'overview':
      return buildAdminError('RELAY_UNAVAILABLE', '总览当前不可读，已自动重试。', true, requestId)
    case 'wechat':
      return buildAdminError(
        'WECHAT_RECONNECT_REQUIRED',
        '微信状态读取失败，请稍后重试。',
        true,
        requestId,
      )
    case 'devices':
      return buildAdminError('INTERNAL', '设备列表读取失败。', true, requestId)
    case 'queue':
      return buildAdminError('OUTBOX_BACKLOG', '队列读取返回降级数据。', true, requestId)
    case 'system':
      return buildAdminError('INTERNAL', '系统快照读取失败。', true, requestId)
  }
}

function maybeFail(
  section: 'overview' | 'wechat' | 'devices' | 'queue' | 'system',
  scenario: ScenarioFixture,
  override: MockAdminReadRepositoryOptions['failureOverride'],
  requestId: string,
): void {
  const overrideFailure = override?.()?.[section]
  const scenarioFailure = scenario.behavior.failureProfile?.[section]
  const status = overrideFailure ?? scenarioFailure
  if (status === 'failed') {
    throw new AdminRepositoryError(sectionFailureError(section, requestId))
  }
}

export class MockAdminReadRepository implements AdminReadRepository {
  private readonly options: Required<
    Pick<
      MockAdminReadRepositoryOptions,
      'clock' | 'getScenarioId' | 'delaySource' | 'requestIdFactory'
    >
  > &
    Omit<
      MockAdminReadRepositoryOptions,
      'clock' | 'getScenarioId' | 'delaySource' | 'requestIdFactory'
    >

  constructor(options: MockAdminReadRepositoryOptions) {
    this.options = {
      clock: options.clock,
      getScenarioId: options.getScenarioId,
      delaySource: options.delaySource ?? Math.random,
      requestIdFactory:
        options.requestIdFactory ?? (() => `mock-req-${Math.random().toString(36).slice(2, 10)}`),
      ...(options.zeroDelay ? { zeroDelay: true } : {}),
      ...(options.failureOverride ? { failureOverride: options.failureOverride } : {}),
      mutationState: options.mutationState,
    }
  }

  private currentScenario(): ScenarioFixture {
    return getScenarioById(this.options.getScenarioId())
  }

  private delayConfig(): { minMs: number; maxMs: number } {
    return this.currentScenario().behavior.delayProfile === 'slow'
      ? SLOW_MOCK_DELAY
      : DEFAULT_MOCK_DELAY
  }

  private async applyDelay(signal?: AbortSignal): Promise<void> {
    if (this.options.zeroDelay) {
      return
    }
    const { minMs, maxMs } = this.delayConfig()
    const span = maxMs - minMs
    const ms = span > 0 ? Math.floor(this.options.delaySource() * span) + minMs : minMs
    await abortableDelay(ms, signal)
  }

  private makeRequestId(): string {
    return this.options.requestIdFactory()
  }

  async getOverview(options?: RequestOptions): Promise<AdminOverview> {
    await this.applyDelay(options?.signal)
    const scenario = this.currentScenario()
    const requestId = this.makeRequestId()
    maybeFail('overview', scenario, this.options.failureOverride, requestId)
    return scenario.data.overview
  }

  async listDevices(
    query: DeviceListQuery,
    options?: RequestOptions,
  ): Promise<Page<DeviceListItem>> {
    await this.applyDelay(options?.signal)
    const scenario = this.currentScenario()
    const requestId = this.makeRequestId()
    maybeFail('devices', scenario, this.options.failureOverride, requestId)
    const limit = query.limit
    const cursor = query.cursor ?? null
    let items = scenario.data.devices.map((item) => {
      const mutation = this.options.mutationState?.devices.get(item.id)
      if (!mutation) return item
      return {
        ...item,
        lastRotatedAt: mutation.lastRotatedAt,
        state: mutation.revoked ? 'revoked' : mutation.enabled ? item.state : 'disabled',
        gatewayState: mutation.revoked || !mutation.enabled ? 'not_enabled' : item.gatewayState,
        updatedAt: this.options.clock.now(),
      }
    })
    for (const [id, input] of this.options.mutationState?.createdDevices ?? []) {
      const mutation = this.options.mutationState?.devices.get(id)
      const revoked = mutation?.revoked ?? false
      const enabled = mutation?.enabled ?? true
      items.push({
        id,
        name: input.name,
        state: revoked ? 'revoked' : enabled ? 'offline' : 'disabled',
        gatewayState: revoked || !enabled ? 'not_enabled' : 'not_enabled',
        scopes: input.scopes,
        tokenFormatVersion: 2,
        lastRotatedAt: mutation?.lastRotatedAt ?? null,
        clientVersion: null,
        lastSeenAt: null,
        createdAt: input.createdAt,
        updatedAt: this.options.clock.now(),
      })
    }
    if (query.state) {
      items = items.filter((d) => d.state === query.state)
    }
    if (query.search) {
      const needle = query.search.toLowerCase()
      items = items.filter(
        (d) => d.name.toLowerCase().includes(needle) || d.id.toLowerCase().includes(needle),
      )
    }
    const page = paginate(items, limit, cursor)
    return { ...page, generatedAt: this.options.clock.now() }
  }

  async getDevice(deviceId: string, options?: RequestOptions): Promise<DeviceDetail> {
    await this.applyDelay(options?.signal)
    const scenario = this.currentScenario()
    const baseDetail = scenario.data.deviceDetails[deviceId]
    const mutation = this.options.mutationState?.devices.get(deviceId)
    const created = this.options.mutationState?.createdDevices.get(deviceId)
    const detail = baseDetail
      ? {
          ...baseDetail,
          ...(mutation
            ? {
                lastRotatedAt: mutation.lastRotatedAt,
                state: mutation.revoked
                  ? 'revoked'
                  : mutation.enabled
                    ? baseDetail.state
                    : 'disabled',
                gatewayState:
                  mutation.revoked || !mutation.enabled ? 'not_enabled' : baseDetail.gatewayState,
                updatedAt: this.options.clock.now(),
              }
            : {}),
        }
      : created
        ? {
            id: deviceId,
            name: created.name,
            state: (mutation?.revoked
              ? 'revoked'
              : mutation && !mutation.enabled
                ? 'disabled'
                : 'offline') as DeviceDetail['state'],
            gatewayState: 'not_enabled' as const,
            scopes: created.scopes,
            tokenFormatVersion: 2,
            lastRotatedAt: mutation?.lastRotatedAt ?? null,
            clientVersion: null,
            lastSeenAt: null,
            createdAt: created.createdAt,
            updatedAt: this.options.clock.now(),
            fullDeviceUuid: `mock-uuid-${deviceId}-0000`,
            capabilities: ['outbox.read'],
            lastHeartbeatAt: null,
            connectedAt: null,
            gatewayGeneration: null,
          }
        : undefined
    if (!detail) {
      throw new AdminRepositoryError(
        buildAdminError('DEVICE_NOT_FOUND', '设备不存在或已删除。', false, this.makeRequestId()),
      )
    }
    return detail
  }

  async getWechatStatus(options?: RequestOptions): Promise<WechatAdminStatus> {
    await this.applyDelay(options?.signal)
    const scenario = this.currentScenario()
    const requestId = this.makeRequestId()
    maybeFail('wechat', scenario, this.options.failureOverride, requestId)
    return scenario.data.wechat
  }

  async listChannelEvents(
    query: ChannelEventQuery,
    options?: RequestOptions,
  ): Promise<Page<ChannelEventItem>> {
    await this.applyDelay(options?.signal)
    const scenario = this.currentScenario()
    const page = paginate(scenario.data.channelEvents, query.limit, query.cursor ?? null)
    return { ...page, generatedAt: this.options.clock.now() }
  }

  async listDeliveries(
    query: DeliveryListQuery,
    options?: RequestOptions,
  ): Promise<Page<DeliveryListItem>> {
    await this.applyDelay(options?.signal)
    const scenario = this.currentScenario()
    const requestId = this.makeRequestId()
    maybeFail('queue', scenario, this.options.failureOverride, requestId)
    let items = scenario.data.deliveries
    if (query.state) {
      items = items.filter((d) => d.state === query.state)
    }
    if (query.origin) {
      items = items.filter((d) => d.origin === query.origin)
    }
    if (query.sinceAt !== undefined) {
      items = items.filter((d) => d.createdAt >= (query.sinceAt ?? 0))
    }
    if (query.untilAt !== undefined) {
      items = items.filter((d) => d.createdAt <= (query.untilAt ?? Number.MAX_SAFE_INTEGER))
    }
    const page = paginate(items, query.limit, query.cursor ?? null)
    return { ...page, generatedAt: this.options.clock.now() }
  }

  async listInteractiveReplies(
    query: InteractiveReplyListQuery,
    options?: RequestOptions,
  ): Promise<Page<InteractiveReplyListItem>> {
    await this.applyDelay(options?.signal)
    const scenario = this.currentScenario()
    const requestId = this.makeRequestId()
    maybeFail('queue', scenario, this.options.failureOverride, requestId)
    let items = scenario.data.replies
    if (query.state) {
      items = items.filter((r) => r.state === query.state)
    }
    const page = paginate(items, query.limit, query.cursor ?? null)
    return { ...page, generatedAt: this.options.clock.now() }
  }

  async listInboundCommands(
    query: InboundCommandListQuery,
    options?: RequestOptions,
  ): Promise<Page<InboundCommandListItem>> {
    await this.applyDelay(options?.signal)
    const scenario = this.currentScenario()
    let items = scenario.data.inbound
    if (query.command) {
      items = items.filter((i) => i.command === query.command)
    }
    if (query.state) {
      items = items.filter((i) => i.state === query.state)
    }
    if (query.sinceAt !== undefined) {
      items = items.filter((i) => i.createdAt >= (query.sinceAt ?? 0))
    }
    const page = paginate(items, query.limit, query.cursor ?? null)
    return { ...page, generatedAt: this.options.clock.now() }
  }

  async listResults(
    _query: ResultListQuery,
    options?: RequestOptions,
  ): Promise<Page<ResultListItem>> {
    await this.applyDelay(options?.signal)
    return { items: [], nextCursor: null, total: 0, generatedAt: this.options.clock.now() }
  }

  async getSystemSnapshot(options?: RequestOptions): Promise<SystemSnapshot> {
    await this.applyDelay(options?.signal)
    const scenario = this.currentScenario()
    const requestId = this.makeRequestId()
    maybeFail('system', scenario, this.options.failureOverride, requestId)
    return scenario.data.system
  }
}
