import { computed, ref, watch } from 'vue'

import { useAdminRepository } from '@/composables/useAdminRepository'
import { useAsyncResource, type AsyncResource } from '@/composables/useAsyncResource'
import { useNow } from '@/composables/useNow'
import { usePageQuery } from '@/composables/usePageQuery'
import type { Page } from '@/contracts/common'
import type { AdminError } from '@/contracts/error'
import type {
  DeliveryListItem,
  DeliveryOrigin,
  DeliveryState,
  InboundCommand,
  InboundCommandListItem,
  InboundState,
  InteractiveReplyListItem,
} from '@/contracts/queue'
import type { AdminRepositoryBundle } from '@/data/create-admin-repository'
import { useCursorPager, type CursorPager } from './useCursorPager'

export type QueueTab = 'deliveries' | 'replies' | 'inbound'

export const QUEUE_TABS: ReadonlyArray<{ id: QueueTab; label: string }> = [
  { id: 'deliveries', label: '通知投递' },
  { id: 'replies', label: '交互回复' },
  { id: 'inbound', label: '入站命令' },
]

export type SelectedQueueRow =
  | { tab: 'deliveries'; row: DeliveryListItem }
  | { tab: 'replies'; row: InteractiveReplyListItem }
  | { tab: 'inbound'; row: InboundCommandListItem }
  | null

const STALE_AFTER_MS = 60_000

export function useQueueController(repository: AdminRepositoryBundle = useAdminRepository()) {
  const query = usePageQuery({
    tab: null,
    dstate: null,
    dorigin: null,
    rstate: null,
    istate: null,
    icommand: null,
    item: null,
  })
  const { now } = useNow()

  const activeTab = computed<QueueTab>({
    get: () => {
      const value = query.values.tab
      return value === 'deliveries' || value === 'replies' || value === 'inbound'
        ? value
        : 'deliveries'
    },
    set: (value) => query.set('tab', value),
  })
  const deliveryState = queryValue<DeliveryState>('dstate')
  const deliveryOrigin = queryValue<DeliveryOrigin>('dorigin')
  const replyState = queryValue<DeliveryState>('rstate')
  const inboundState = queryValue<InboundState>('istate')
  const inboundCommand = queryValue<InboundCommand>('icommand')

  function queryValue<T extends string>(key: string) {
    return computed<T | null>({
      get: () => (query.values[key] as T | null) ?? null,
      set: (value) => query.set(key, value),
    })
  }

  const deliveryPager = useCursorPager()
  const replyPager = useCursorPager()
  const inboundPager = useCursorPager()

  const deliveriesResource = useAsyncResource<Page<DeliveryListItem>>({
    immediate: false,
    fetcher: (signal) =>
      repository.read.listDeliveries(
        {
          limit: 50,
          cursor: deliveryPager.cursor.value,
          ...(deliveryState.value ? { state: deliveryState.value } : {}),
          ...(deliveryOrigin.value ? { origin: deliveryOrigin.value } : {}),
        },
        { signal },
      ),
  })
  const repliesResource = useAsyncResource<Page<InteractiveReplyListItem>>({
    immediate: false,
    fetcher: (signal) =>
      repository.read.listInteractiveReplies(
        {
          limit: 50,
          cursor: replyPager.cursor.value,
          ...(replyState.value ? { state: replyState.value } : {}),
        },
        { signal },
      ),
  })
  const inboundResource = useAsyncResource<Page<InboundCommandListItem>>({
    immediate: false,
    fetcher: (signal) =>
      repository.read.listInboundCommands(
        {
          limit: 50,
          cursor: inboundPager.cursor.value,
          ...(inboundCommand.value ? { command: inboundCommand.value } : {}),
          ...(inboundState.value ? { state: inboundState.value } : {}),
        },
        { signal },
      ),
  })

  watch([deliveryState, deliveryOrigin], () => {
    deliveryPager.reset()
    void deliveriesResource.refresh('initial')
  })
  watch(replyState, () => {
    replyPager.reset()
    void repliesResource.refresh('initial')
  })
  watch([inboundState, inboundCommand], () => {
    inboundPager.reset()
    void inboundResource.refresh('initial')
  })

  function recoverCursorPage<T>(
    pager: CursorPager,
    resource: AsyncResource<T>,
    error: AdminError | null,
  ): void {
    if (pager.recoverFromCursorError(error)) {
      void resource.refresh('initial')
    }
  }

  watch(deliveriesResource.error, (error) =>
    recoverCursorPage(deliveryPager, deliveriesResource, error),
  )
  watch(repliesResource.error, (error) => recoverCursorPage(replyPager, repliesResource, error))
  watch(inboundResource.error, (error) => recoverCursorPage(inboundPager, inboundResource, error))

  function ensureTabLoaded(tab: QueueTab): void {
    const resource =
      tab === 'deliveries'
        ? deliveriesResource
        : tab === 'replies'
          ? repliesResource
          : inboundResource
    if (resource.data.value === null || resource.isStale(STALE_AFTER_MS, now.value)) {
      void resource.refresh(resource.data.value === null ? 'initial' : 'refresh')
    }
  }
  watch(activeTab, ensureTabLoaded, { immediate: true })

  const deliveryRows = computed(() => deliveriesResource.data.value?.items ?? [])
  const replyRows = computed(() => repliesResource.data.value?.items ?? [])
  const inboundRows = computed(() => inboundResource.data.value?.items ?? [])

  const activeResource = computed(() => {
    if (activeTab.value === 'deliveries') return deliveriesResource
    if (activeTab.value === 'replies') return repliesResource
    return inboundResource
  })
  const activeError = computed(() => activeResource.value.error.value)
  const activeLoading = computed(
    () => activeResource.value.loading.value && activeResource.value.data.value === null,
  )
  const activeRefreshing = computed(() => activeResource.value.refreshing.value)
  const activeHasUsableData = computed(() => activeResource.value.data.value !== null)

  async function refreshActive(): Promise<void> {
    await activeResource.value.refresh('refresh')
  }

  function nextDelivery(): void {
    if (deliveryPager.next(deliveriesResource.data.value?.nextCursor ?? null)) {
      void deliveriesResource.refresh('refresh')
    }
  }
  function previousDelivery(): void {
    if (deliveryPager.previous()) void deliveriesResource.refresh('refresh')
  }
  function nextReply(): void {
    if (replyPager.next(repliesResource.data.value?.nextCursor ?? null)) {
      void repliesResource.refresh('refresh')
    }
  }
  function previousReply(): void {
    if (replyPager.previous()) void repliesResource.refresh('refresh')
  }
  function nextInbound(): void {
    if (inboundPager.next(inboundResource.data.value?.nextCursor ?? null)) {
      void inboundResource.refresh('refresh')
    }
  }
  function previousInbound(): void {
    if (inboundPager.previous()) void inboundResource.refresh('refresh')
  }

  const deliveryFreshness = freshnessFor(deliveriesResource)
  const replyFreshness = freshnessFor(repliesResource)
  const inboundFreshness = freshnessFor(inboundResource)

  function freshnessFor<T>(resource: AsyncResource<T>) {
    return computed(() => {
      const lastSuccessAt = resource.lastSuccessAt.value
      if (lastSuccessAt === null) return '尚未加载'
      if (resource.error.value !== null && resource.data.value !== null) {
        return `刷新失败 · 数据为 ${humanizeAgo(lastSuccessAt, now.value)}`
      }
      if (now.value - lastSuccessAt > STALE_AFTER_MS) {
        return `数据可能已过期 · ${humanizeAgo(lastSuccessAt, now.value)}`
      }
      return `最近刷新：${humanizeAgo(lastSuccessAt, now.value)}`
    })
  }

  const selectedRow = ref<SelectedQueueRow>(null)
  const selectedItemId = computed<string | null>({
    get: () => query.values.item ?? null,
    set: (value) => query.set('item', value, 'push'),
  })
  const selectedItemMissing = computed(
    () => selectedItemId.value !== null && activeHasUsableData.value && selectedRow.value === null,
  )
  const drawerOpen = computed<boolean>({
    get: () => selectedItemId.value !== null,
    set: (open) => {
      if (!open) selectedItemId.value = null
    },
  })

  function openDelivery(row: DeliveryListItem): void {
    selectedItemId.value = row.id
  }
  function openReply(row: InteractiveReplyListItem): void {
    selectedItemId.value = row.id
  }
  function openInbound(row: InboundCommandListItem): void {
    selectedItemId.value = row.id
  }
  function restoreDeepLinkedItem(): void {
    const id = selectedItemId.value
    if (!id) {
      selectedRow.value = null
      return
    }
    const rows =
      activeTab.value === 'deliveries'
        ? deliveryRows.value
        : activeTab.value === 'replies'
          ? replyRows.value
          : inboundRows.value
    const found = rows.find((row) => row.id === id)
    if (!found) {
      if (activeHasUsableData.value) selectedRow.value = null
      return
    }
    selectedRow.value =
      activeTab.value === 'deliveries'
        ? { tab: 'deliveries', row: found as DeliveryListItem }
        : activeTab.value === 'replies'
          ? { tab: 'replies', row: found as InteractiveReplyListItem }
          : { tab: 'inbound', row: found as InboundCommandListItem }
  }
  watch([selectedItemId, deliveryRows, replyRows, inboundRows, activeTab], restoreDeepLinkedItem, {
    immediate: true,
  })

  function clearActiveFilters(): void {
    if (activeTab.value === 'deliveries') {
      deliveryState.value = null
      deliveryOrigin.value = null
    } else if (activeTab.value === 'replies') {
      replyState.value = null
    } else {
      inboundState.value = null
      inboundCommand.value = null
    }
  }

  return {
    activeTab,
    deliveryState,
    deliveryOrigin,
    replyState,
    inboundState,
    inboundCommand,
    deliveriesResource,
    repliesResource,
    inboundResource,
    deliveryRows,
    replyRows,
    inboundRows,
    activeError,
    activeLoading,
    activeRefreshing,
    activeHasUsableData,
    refreshActive,
    nextDelivery,
    previousDelivery,
    nextReply,
    previousReply,
    nextInbound,
    previousInbound,
    deliveryPager,
    replyPager,
    inboundPager,
    deliveryFreshness,
    replyFreshness,
    inboundFreshness,
    selectedRow,
    selectedItemMissing,
    drawerOpen,
    openDelivery,
    openReply,
    openInbound,
    clearActiveFilters,
  }
}

function humanizeAgo(timestamp: number, now: number): string {
  const difference = now - timestamp
  if (difference < 5_000) return '刚刚'
  if (difference < 60_000) return `${Math.floor(difference / 1_000)} 秒前`
  if (difference < 60 * 60_000) return `${Math.floor(difference / 60_000)} 分钟前`
  if (difference < 24 * 60 * 60_000) return `${Math.floor(difference / (60 * 60_000))} 小时前`
  return `${Math.floor(difference / (24 * 60 * 60_000))} 天前`
}
