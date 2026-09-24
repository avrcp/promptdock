import { inject, provide, ref, type InjectionKey } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

export type HoldStatus = {
  revision: number
  startedAt: number | null
  until: number | null
  requestedDuration: number | null
  state: 'active' | 'inactive' | 'uncertain'
}
type HoldReceipt = { status: 'applied' | 'conflict'; state: HoldStatus }
const EVENT = 'notification-hold-changed'

function isStatus(value: unknown): value is HoldStatus {
  return (
    typeof value === 'object' &&
    value !== null &&
    !Array.isArray(value) &&
    Number.isSafeInteger((value as HoldStatus).revision) &&
    ['active', 'inactive', 'uncertain'].includes((value as HoldStatus).state) &&
    ['startedAt', 'until', 'requestedDuration'].every((key) => {
      const field = (value as Record<string, unknown>)[key]
      return field === null || (typeof field === 'number' && Number.isSafeInteger(field))
    })
  )
}
function isReceipt(value: unknown): value is HoldReceipt {
  return (
    typeof value === 'object' &&
    value !== null &&
    ['applied', 'conflict'].includes((value as HoldReceipt).status) &&
    isStatus((value as HoldReceipt).state)
  )
}

export function createNotificationHoldController() {
  const status = ref<HoldStatus | null>(null)
  const pending = ref(false)
  const error = ref('')
  let disposed = false
  let reading: Promise<void> | null = null
  let dirty = false
  let epoch = 0
  let unlisten: UnlistenFn | null = null
  let timer: ReturnType<typeof setInterval> | null = null
  let onFocus: (() => void) | null = null
  let mutationEpoch = 0
  let starting = false
  function accept(next: HoldStatus) {
    if (
      !status.value ||
      next.state === 'uncertain' ||
      status.value.state === 'uncertain' ||
      next.revision >= status.value.revision
    )
      status.value = next
  }

  async function refresh(preserveError = false) {
    if (disposed) return
    if (reading) {
      dirty = true
      return reading
    }
    reading = (async () => {
      do {
        dirty = false
        const current = ++epoch
        try {
          const next = await invoke<unknown>('desktop_notification_hold_status')
          if (disposed || current !== epoch) continue
          if (!isStatus(next)) throw new Error('通知暂缓状态回执无法确认。')
          accept(next)
          if (!preserveError) error.value = ''
        } catch (reason) {
          if (!disposed)
            error.value = reason instanceof Error ? reason.message : '通知暂缓状态暂不可读取。'
        }
      } while (dirty && !disposed)
    })().finally(() => {
      reading = null
    })
    return reading
  }

  async function mutate(
    command: 'desktop_notification_hold_set' | 'desktop_notification_hold_resume',
    args: Record<string, unknown>,
  ) {
    if (disposed || pending.value || !status.value) return
    pending.value = true
    error.value = ''
    const current = ++mutationEpoch
    try {
      const receipt = await invoke<unknown>(command, args)
      if (!isReceipt(receipt)) throw new Error('通知暂缓操作回执无法确认，正在重新读取状态。')
      if (disposed || current !== mutationEpoch) return
      accept(receipt.state)
      if (receipt.status === 'conflict')
        error.value = '状态已在其他位置变化；已显示实际通知暂缓状态。'
    } catch (reason) {
      if (disposed || current !== mutationEpoch) return
      error.value =
        reason instanceof Error ? reason.message : '通知暂缓操作未确认，正在重新读取状态。'
      await refresh(true)
    } finally {
      if (!disposed && current === mutationEpoch) pending.value = false
    }
  }
  async function hold(minutes: 15 | 60) {
    if (status.value)
      await mutate('desktop_notification_hold_set', {
        minutes,
        expectedRevision: status.value.revision,
      })
  }
  async function resume() {
    if (status.value)
      await mutate('desktop_notification_hold_resume', { expectedRevision: status.value.revision })
  }
  async function start() {
    if (disposed || starting || unlisten || onFocus) return
    starting = true
    try {
      const lateUnlisten = await listen(EVENT, () => void refresh())
      if (disposed) {
        await lateUnlisten()
        return
      }
      unlisten = lateUnlisten
    } catch {
      /* fallback below */
    }
    if (disposed) return
    onFocus = () => void refresh()
    window.addEventListener('focus', onFocus)
    timer = setInterval(() => void refresh(), 30_000)
    await refresh()
  }
  function dispose() {
    disposed = true
    epoch++
    mutationEpoch++
    if (timer) clearInterval(timer)
    timer = null
    if (onFocus) window.removeEventListener('focus', onFocus)
    onFocus = null
    if (unlisten) void unlisten()
    unlisten = null
  }
  return { status, pending, error, refresh, hold, resume, start, dispose }
}
export type NotificationHoldController = ReturnType<typeof createNotificationHoldController>
const key = Symbol('promptdock:notification-hold') as InjectionKey<NotificationHoldController>
export function provideNotificationHoldController() {
  const controller = createNotificationHoldController()
  provide(key, controller)
  return controller
}
export function useNotificationHoldController() {
  const controller = inject(key, null)
  if (!controller) throw new Error('useNotificationHoldController 必须在 provider 后代调用')
  return controller
}
