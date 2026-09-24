import { z } from 'zod'
import {
  adminOverviewWireSchema,
  deviceDetailWireSchema,
  devicesPageWireSchema,
  devicesQueryWireSchema,
  deliveriesPageWireSchema,
  deliveriesQueryWireSchema,
  inboundCommandsPageWireSchema,
  inboundCommandsQueryWireSchema,
  resultsPageWireSchema,
  resultsQueryWireSchema,
  interactiveRepliesPageWireSchema,
  interactiveRepliesQueryWireSchema,
  systemWireSchema,
  wechatEventsPageWireSchema,
  wechatEventsQueryWireSchema,
  wechatStatusWireSchema,
  type AdminMeta,
} from '@promptdock/relay-admin-api-generated'

import type { AdminReadRepository } from '@/data/admin-read-repository'
import type { RequestOptions, Page } from '@/contracts/common'
import type { AdminOverview } from '@/contracts/overview'
import type { DeviceDetail, DeviceListItem, DeviceListQuery } from '@/contracts/device'
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
import type { ResultListItem, ResultListQuery } from '@/contracts/result'
import { AdminRepositoryError } from '@/contracts/error'

import { mapHttpError } from './error-mapper'
import { AdminCapabilityMetadata } from './admin-capability-metadata'
import { HttpClient, HttpClientError } from './http-client'
import {
  mapDeviceDetail,
  mapDevicesPage,
  mapDeliveriesPage,
  mapInboundCommandsPage,
  mapInteractiveRepliesPage,
  mapOverview,
  mapSystem,
  mapWechatEventsPage,
  mapWechatStatus,
} from './mapper'

const REQUIRED_READ_CAPABILITY = 'admin_read_v2'
const MAX_RELAY_PAGE_SIZE = 100
const DEFAULT_RELAY_PAGE_SIZE = 50
const PAGE_SIZE_KNOWLEDGE_TTL_MS = 60_000

type ReadQuery = Record<string, string | number | boolean | readonly string[] | null | undefined>
type PageQuery = ReadQuery & { limit: number; cursor?: string | null }

export interface HttpAdminReadRepositoryOptions {
  client?: HttpClient
  capabilityMetadata?: AdminCapabilityMetadata
  pageSizeKnowledgeTtlMs?: number
  now?: () => number
}

export class HttpAdminReadRepository implements AdminReadRepository {
  private readonly client: HttpClient
  private readonly capabilityMetadata: AdminCapabilityMetadata
  private readonly pageSizeKnowledgeTtlMs: number
  private readonly now: () => number
  private maxAcceptedPageSize = 0
  private pageSizeCeiling: number | null = null
  private pageSizeKnowledgeAt: number | null = null
  /**
   * Relay accepts a deployment-specific admin.max_page_size but /meta does not
   * advertise it. Successful unfiltered probes provide only a lower bound;
   * later larger requests probe again so Relay restarts/config changes recover.
   */

  constructor(options: HttpAdminReadRepositoryOptions = {}) {
    this.client = options.client ?? new HttpClient()
    this.capabilityMetadata =
      options.capabilityMetadata ?? new AdminCapabilityMetadata({ client: this.client })
    this.pageSizeKnowledgeTtlMs = options.pageSizeKnowledgeTtlMs ?? PAGE_SIZE_KNOWLEDGE_TTL_MS
    this.now = options.now ?? Date.now
    if (!Number.isSafeInteger(this.pageSizeKnowledgeTtlMs) || this.pageSizeKnowledgeTtlMs <= 0) {
      throw new RangeError('page-size knowledge TTL must be a positive safe integer')
    }
  }

  async getMeta(options?: RequestOptions): Promise<AdminMeta> {
    try {
      return await this.capabilityMetadata.get({ force: options?.force })
    } catch (error) {
      throw toRepositoryError(error)
    }
  }

  async getOverview(options?: RequestOptions): Promise<AdminOverview> {
    return this.read('/overview', adminOverviewWireSchema, mapOverview, options?.signal)
  }

  async listDevices(
    query: DeviceListQuery,
    options?: RequestOptions,
  ): Promise<Page<DeviceListItem>> {
    const parsed = normalizePageQuery(parseQuery(devicesQueryWireSchema, query))
    return this.readPage('/devices', devicesPageWireSchema, mapDevicesPage, options?.signal, parsed)
  }

  async getDevice(deviceId: string, options?: RequestOptions): Promise<DeviceDetail> {
    if (deviceId.length === 0) {
      throw invalidQueryError()
    }
    return this.read(
      `/devices/${encodeURIComponent(deviceId)}`,
      deviceDetailWireSchema,
      mapDeviceDetail,
      options?.signal,
    )
  }

  async getWechatStatus(options?: RequestOptions): Promise<WechatAdminStatus> {
    return this.read('/wechat/status', wechatStatusWireSchema, mapWechatStatus, options?.signal)
  }

  async listChannelEvents(
    query: ChannelEventQuery,
    options?: RequestOptions,
  ): Promise<Page<ChannelEventItem>> {
    const parsed = normalizePageQuery(parseQuery(wechatEventsQueryWireSchema, query))
    return this.readPage(
      '/wechat/events',
      wechatEventsPageWireSchema,
      mapWechatEventsPage,
      options?.signal,
      parsed,
    )
  }

  async listDeliveries(
    query: DeliveryListQuery,
    options?: RequestOptions,
  ): Promise<Page<DeliveryListItem>> {
    const parsed = normalizePageQuery(parseQuery(deliveriesQueryWireSchema, query))
    return this.readPage(
      '/deliveries',
      deliveriesPageWireSchema,
      mapDeliveriesPage,
      options?.signal,
      parsed,
    )
  }

  async listInteractiveReplies(
    query: InteractiveReplyListQuery,
    options?: RequestOptions,
  ): Promise<Page<InteractiveReplyListItem>> {
    const parsed = normalizePageQuery(parseQuery(interactiveRepliesQueryWireSchema, query))
    return this.readPage(
      '/interactive-replies',
      interactiveRepliesPageWireSchema,
      mapInteractiveRepliesPage,
      options?.signal,
      parsed,
    )
  }

  async listInboundCommands(
    query: InboundCommandListQuery,
    options?: RequestOptions,
  ): Promise<Page<InboundCommandListItem>> {
    const parsed = normalizePageQuery(parseQuery(inboundCommandsQueryWireSchema, query))
    return this.readPage(
      '/inbound-commands',
      inboundCommandsPageWireSchema,
      mapInboundCommandsPage,
      options?.signal,
      parsed,
    )
  }

  async listResults(
    query: ResultListQuery,
    options?: RequestOptions,
  ): Promise<Page<ResultListItem>> {
    const parsed = normalizePageQuery(parseQuery(resultsQueryWireSchema, query))
    return this.readPage(
      '/results',
      resultsPageWireSchema,
      (value: unknown) => {
        const page = resultsPageWireSchema.parse(value)
        return {
          items: page.items,
          nextCursor: page.nextCursor ?? null,
          total: page.total ?? null,
          generatedAt: page.generatedAt,
        }
      },
      options?.signal,
      parsed,
    )
  }

  async getSystemSnapshot(options?: RequestOptions): Promise<SystemSnapshot> {
    return this.read('/system', systemWireSchema, mapSystem, options?.signal)
  }

  private async read<TWire, TView>(
    path: string,
    schema: z.ZodType<TWire>,
    mapper: (value: unknown) => TView,
    signal?: AbortSignal,
    query?: ReadQuery,
  ): Promise<TView> {
    try {
      return await this.readUnchecked(path, schema, mapper, signal, query)
    } catch (error) {
      throw toRepositoryError(error)
    }
  }

  private async readPage<TWire, TView>(
    path: string,
    schema: z.ZodType<TWire>,
    mapper: (value: unknown) => TView,
    signal: AbortSignal | undefined,
    query: PageQuery,
  ): Promise<TView> {
    try {
      this.expirePageSizeKnowledge()
      const requestedLimit = Math.min(query.limit, MAX_RELAY_PAGE_SIZE)
      if (this.pageSizeCeiling !== null || requestedLimit <= this.maxAcceptedPageSize) {
        const knownLimit =
          this.pageSizeCeiling === null
            ? requestedLimit
            : Math.min(requestedLimit, this.pageSizeCeiling)
        return await this.readWithKnownPageSize(path, schema, mapper, signal, query, knownLimit)
      }

      const probe = await this.discoverPageSize(
        path,
        schema,
        mapper,
        signal,
        requestedLimit,
        this.maxAcceptedPageSize,
      )
      this.recordPageSizeProbe(probe)
      if (isUnfilteredFirstPage(query)) {
        return probe.page
      }

      return await this.readUnchecked(path, schema, mapper, signal, {
        ...query,
        limit: probe.limit,
      })
    } catch (error) {
      throw toRepositoryError(error)
    }
  }

  private async readWithKnownPageSize<TWire, TView>(
    path: string,
    schema: z.ZodType<TWire>,
    mapper: (value: unknown) => TView,
    signal: AbortSignal | undefined,
    query: PageQuery,
    knownLimit: number,
  ): Promise<TView> {
    try {
      return await this.readUnchecked(path, schema, mapper, signal, {
        ...query,
        limit: knownLimit,
      })
    } catch (error) {
      if (!isPageSizeValidationFailure(error)) throw error

      const refreshed = await this.discoverPageSize(path, schema, mapper, signal, knownLimit)
      if (!refreshed.bounded) {
        if (isUnfilteredFirstPage(query)) {
          this.recordPageSizeProbe(refreshed)
          return refreshed.page
        }
        throw error
      }

      this.recordPageSizeProbe(refreshed)
      if (isUnfilteredFirstPage(query)) return refreshed.page
      return await this.readUnchecked(path, schema, mapper, signal, {
        ...query,
        limit: refreshed.limit,
      })
    }
  }

  private async discoverPageSize<TWire, TView>(
    path: string,
    schema: z.ZodType<TWire>,
    mapper: (value: unknown) => TView,
    signal: AbortSignal | undefined,
    requestedLimit: number,
    acceptedFloor = 0,
  ): Promise<{ limit: number; page: TView; bounded: boolean }> {
    try {
      const page = await this.readUnchecked(path, schema, mapper, signal, {
        limit: requestedLimit,
        cursor: null,
      })
      return { limit: requestedLimit, page, bounded: false }
    } catch (error) {
      if (!isPageSizeValidationFailure(error)) throw error
    }

    let rejectedUpper = requestedLimit
    let candidate =
      acceptedFloor > 0 && acceptedFloor < rejectedUpper
        ? acceptedFloor
        : nextSmallerPageSize(rejectedUpper)
    let accepted: { limit: number; page: TView } | null = null

    while (candidate !== null) {
      try {
        const page = await this.readUnchecked(path, schema, mapper, signal, {
          limit: candidate,
          cursor: null,
        })
        accepted = { limit: candidate, page }
        break
      } catch (error) {
        const smallerLimit = nextSmallerPageSize(candidate)
        if (!isPageSizeValidationFailure(error) || smallerLimit === null) throw error
        rejectedUpper = candidate
        candidate = smallerLimit
      }
    }

    if (!accepted) throw new Error('Relay rejected every valid Admin page size.')
    let upper = rejectedUpper - 1
    while (accepted.limit < upper) {
      const midpoint = Math.ceil((accepted.limit + upper) / 2)
      try {
        const page = await this.readUnchecked(path, schema, mapper, signal, {
          limit: midpoint,
          cursor: null,
        })
        accepted = { limit: midpoint, page }
      } catch (error) {
        if (!isPageSizeValidationFailure(error)) throw error
        upper = midpoint - 1
      }
    }
    return { ...accepted, bounded: true }
  }

  private recordPageSizeProbe(probe: { limit: number; bounded: boolean }): void {
    this.maxAcceptedPageSize = probe.bounded
      ? probe.limit
      : Math.max(this.maxAcceptedPageSize, probe.limit)
    this.pageSizeCeiling = probe.bounded ? probe.limit : null
    this.pageSizeKnowledgeAt = this.now()
  }

  private expirePageSizeKnowledge(): void {
    if (
      this.pageSizeKnowledgeAt !== null &&
      this.now() - this.pageSizeKnowledgeAt >= this.pageSizeKnowledgeTtlMs
    ) {
      this.maxAcceptedPageSize = 0
      this.pageSizeCeiling = null
      this.pageSizeKnowledgeAt = null
    }
  }

  private async readUnchecked<TWire, TView>(
    path: string,
    schema: z.ZodType<TWire>,
    mapper: (value: unknown) => TView,
    signal?: AbortSignal,
    query?: ReadQuery,
  ): Promise<TView> {
    await this.ensureReadCapability(signal)
    const response = await this.client.request({ path, query, signal, schema, purpose: 'read' })
    return mapper(response.data)
  }

  private async ensureReadCapability(signal?: AbortSignal): Promise<void> {
    const meta = await this.getMeta({ signal })
    if (!meta.capabilities.includes(REQUIRED_READ_CAPABILITY)) {
      throw new AdminRepositoryError({
        code: 'INTERNAL',
        message: 'Relay Admin 未声明只读能力。',
        retryable: false,
        requestId: null,
      })
    }
  }
}

function parseQuery<T extends z.ZodTypeAny>(schema: T, query: unknown): z.infer<T> {
  try {
    const normalized =
      query && typeof query === 'object' && !Array.isArray(query)
        ? Object.fromEntries(Object.entries(query).filter(([, value]) => value != null))
        : query
    return schema.parse(normalized)
  } catch {
    throw invalidQueryError()
  }
}

function normalizePageQuery<
  T extends ReadQuery & { limit?: number | null; cursor?: string | null },
>(query: T): T & PageQuery {
  return {
    ...query,
    limit: query.limit ?? DEFAULT_RELAY_PAGE_SIZE,
    cursor: query.cursor ?? null,
  }
}

function toRepositoryError(error: unknown): Error {
  if (error instanceof AdminRepositoryError) {
    return error
  }
  if (error instanceof DOMException && error.name === 'AbortError') {
    return error
  }
  if (error instanceof HttpClientError) {
    return new AdminRepositoryError(mapHttpError(error))
  }
  if (error instanceof z.ZodError) {
    return new AdminRepositoryError({
      code: 'INTERNAL',
      message: 'Relay Admin 返回了不符合合同的响应。',
      retryable: false,
      requestId: null,
    })
  }
  return new AdminRepositoryError({
    code: 'INTERNAL',
    message: '请求失败，请稍后重试。',
    retryable: false,
    requestId: null,
  })
}

function invalidQueryError(): AdminRepositoryError {
  return new AdminRepositoryError({
    code: 'INVALID_INPUT',
    message: '查询参数无效。',
    retryable: false,
    requestId: null,
  })
}

function isPageSizeValidationFailure(error: unknown): boolean {
  return (
    error instanceof HttpClientError &&
    error.status === 400 &&
    error.responseCode === 'ADMIN_VALIDATION_FAILED'
  )
}

function nextSmallerPageSize(limit: number): number | null {
  if (limit <= 1) return null
  return Math.max(1, Math.floor(limit / 2))
}

function isUnfilteredFirstPage(query: PageQuery): boolean {
  return Object.entries(query).every(
    ([key, value]) =>
      key === 'limit' ||
      value === null ||
      value === undefined ||
      (Array.isArray(value) && value.length === 0),
  )
}
