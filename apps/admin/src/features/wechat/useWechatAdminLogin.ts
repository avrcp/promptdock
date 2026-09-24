import { computed, onBeforeUnmount, ref } from 'vue'

import { useNow } from '@/composables/useNow'

import type { AdminCommandRepository } from '@/data/admin-command-repository'
import { toAdminError, type AdminError } from '@/contracts/error'
import type { StartWechatLoginInput, WechatLoginSession } from '@/contracts/wechat'

const TERMINAL_LOGIN_STATES = new Set([
  'confirmed',
  'already_connected',
  'expired',
  'cancelled',
  'failed',
])

const POLL_INTERVAL_MS = 1_000
const POLL_RETRY_DELAYS_MS = [1_000, 2_000, 4_000, 5_000]
const POLL_RETRY_GRACE_MS = 5_000

function asAdminError(error: unknown, fallback: string): AdminError {
  return toAdminError(error, {
    code: 'INTERNAL',
    message: fallback,
    retryable: true,
    requestId: null,
  })
}

function retainSafeSession(next: WechatLoginSession): WechatLoginSession {
  // A terminal session has no legitimate browser-side need for QR data.  Do
  // not keep it around merely because a server implementation included it.
  if (!TERMINAL_LOGIN_STATES.has(next.state)) return next
  return {
    loginId: next.loginId,
    state: next.state,
    expiresAt: next.expiresAt,
    canSubmitVerifyCode: next.canSubmitVerifyCode,
    ...(next.errorCode === undefined ? {} : { errorCode: next.errorCode }),
  }
}

/**
 * Owns the ephemeral browser side of an authorized QR session.  The epoch
 * fence means a late start/poll/verify response from a cancelled dialog can
 * never repopulate the next dialog instance.
 */
export function useWechatAdminLogin(command: AdminCommandRepository) {
  const loginOpen = ref(false)
  const session = ref<WechatLoginSession | null>(null)
  const error = ref<AdminError | null>(null)
  const inFlight = ref(false)
  const { now: nowMs } = useNow()
  const verifyCode = ref('')

  let lifecycleEpoch = 0
  let operationSequence = 0
  let pollTimer: ReturnType<typeof setTimeout> | null = null
  let expiryTimer: ReturnType<typeof setTimeout> | null = null
  let controller: AbortController | null = null
  // Consecutive network-error count.  Reset on any successful poll so the
  // operator sees a real "stuck" state only after repeated failures, not
  // after a single Wi-Fi hiccup.
  let consecutivePollFailures = 0
  let isPolling = false

  const state = computed(() => session.value?.state ?? 'fetching_qr')
  const isTerminal = computed(() => TERMINAL_LOGIN_STATES.has(state.value))
  const secondsLeft = computed(() => {
    if (!session.value) return 0
    return Math.max(0, Math.ceil((session.value.expiresAt - nowMs.value) / 1_000))
  })

  function stopPolling(): void {
    if (pollTimer !== null) {
      clearTimeout(pollTimer)
      pollTimer = null
    }
  }

  function stopExpiryTimer(): void {
    if (expiryTimer !== null) {
      clearTimeout(expiryTimer)
      expiryTimer = null
    }
  }

  function invalidateRequest(): void {
    operationSequence += 1
    controller?.abort()
    controller = null
  }

  function beginRequest(): { sequence: number; controller: AbortController } {
    invalidateRequest()
    const requestController = new AbortController()
    controller = requestController
    return { sequence: operationSequence, controller: requestController }
  }

  function isCurrent(
    expectedLifecycleEpoch: number,
    expectedOperationSequence: number,
    requestController: AbortController,
  ): boolean {
    return (
      expectedLifecycleEpoch === lifecycleEpoch &&
      expectedOperationSequence === operationSequence &&
      !requestController.signal.aborted
    )
  }

  function cancelLateLogin(loginId: string): void {
    // A cancelled browser request can still reach Relay.  Once its response
    // gives us an id, retire that server-side candidate without restoring UI
    // state or reusing the aborted signal.
    cancelBestEffort(loginId)
  }

  function clearSensitiveState(): void {
    verifyCode.value = ''
    session.value = null
  }

  function retainTerminalSession(
    current: WechatLoginSession,
    state: Extract<WechatLoginSession['state'], 'expired' | 'failed'>,
  ): WechatLoginSession {
    return retainSafeSession({
      ...current,
      state,
      canSubmitVerifyCode: false,
    })
  }

  function cancelBestEffort(loginId: string): void {
    try {
      void command.cancelWechatLogin(loginId).catch(() => undefined)
    } catch {
      // The browser lifecycle must remain closed even if a repository adapter
      // breaks its promise contract while this best-effort cleanup runs.
    }
  }

  function expireLocally(expectedLifecycleEpoch: number): void {
    const active = session.value
    if (
      expectedLifecycleEpoch !== lifecycleEpoch ||
      !loginOpen.value ||
      !active ||
      TERMINAL_LOGIN_STATES.has(active.state) ||
      Date.now() < active.expiresAt
    ) {
      return
    }

    // A locally expired session is a new lifecycle. Late responses from an
    // aborted start, poll, or verify cannot restore the retired QR payload.
    lifecycleEpoch += 1
    stopPolling()
    stopExpiryTimer()
    invalidateRequest()
    consecutivePollFailures = 0
    inFlight.value = false
    verifyCode.value = ''
    session.value = retainTerminalSession(active, 'expired')
    error.value = null
    cancelBestEffort(active.loginId)
  }

  function failNonRetryable(expectedLifecycleEpoch: number, mapped: AdminError): void {
    const active = session.value
    if (
      expectedLifecycleEpoch !== lifecycleEpoch ||
      !loginOpen.value ||
      !active ||
      TERMINAL_LOGIN_STATES.has(active.state)
    ) {
      return
    }

    lifecycleEpoch += 1
    stopPolling()
    stopExpiryTimer()
    invalidateRequest()
    consecutivePollFailures = 0
    inFlight.value = false
    verifyCode.value = ''
    session.value = retainTerminalSession(active, 'failed')
    error.value = mapped
    cancelBestEffort(active.loginId)
  }

  function armExpiry(expectedLifecycleEpoch: number): void {
    stopExpiryTimer()
    const active = session.value
    if (!loginOpen.value || !active || TERMINAL_LOGIN_STATES.has(active.state)) return

    const delay = active.expiresAt - Date.now()
    if (delay <= 0) {
      expireLocally(expectedLifecycleEpoch)
      return
    }
    expiryTimer = setTimeout(() => {
      expiryTimer = null
      expireLocally(expectedLifecycleEpoch)
    }, delay)
  }

  function schedulePoll(expectedLifecycleEpoch: number): void {
    stopPolling()
    const active = session.value
    if (!loginOpen.value || !active || TERMINAL_LOGIN_STATES.has(active.state)) return
    if (Date.now() >= active.expiresAt) {
      expireLocally(expectedLifecycleEpoch)
      return
    }
    armExpiry(expectedLifecycleEpoch)
    const delay = computePollDelay(consecutivePollFailures)
    const retryDeadline = active.expiresAt + POLL_RETRY_GRACE_MS
    const boundedDelay = Math.min(delay, Math.max(0, retryDeadline - Date.now()))
    if (boundedDelay <= 0) {
      expireLocally(expectedLifecycleEpoch)
      return
    }
    pollTimer = setTimeout(() => {
      void poll(expectedLifecycleEpoch)
    }, boundedDelay)
  }

  function computePollDelay(failures: number): number {
    if (failures <= 0) return POLL_INTERVAL_MS
    const idx = Math.min(failures - 1, POLL_RETRY_DELAYS_MS.length - 1)
    return POLL_RETRY_DELAYS_MS[idx]!
  }

  async function poll(expectedLifecycleEpoch: number): Promise<void> {
    if (
      expectedLifecycleEpoch !== lifecycleEpoch ||
      !loginOpen.value ||
      !session.value ||
      isPolling ||
      TERMINAL_LOGIN_STATES.has(session.value.state)
    ) {
      return
    }
    const activeSession = session.value
    if (Date.now() >= activeSession.expiresAt) {
      expireLocally(expectedLifecycleEpoch)
      return
    }
    const request = beginRequest()
    isPolling = true
    try {
      const next = await command.getWechatLogin(activeSession.loginId, {
        signal: request.controller.signal,
      })
      if (!isCurrent(expectedLifecycleEpoch, request.sequence, request.controller)) return
      session.value = retainSafeSession(next)
      nowMs.value = Date.now()
      consecutivePollFailures = 0
      // A successful read should not leave the prior network error on
      // screen if the most recent attempt recovered.  Clear so the
      // operator does not see "登录失败" with a confirmed session below.
      error.value = null
      if (TERMINAL_LOGIN_STATES.has(next.state)) {
        stopPolling()
        stopExpiryTimer()
        verifyCode.value = ''
      } else {
        schedulePoll(expectedLifecycleEpoch)
      }
    } catch (cause) {
      if (isCurrent(expectedLifecycleEpoch, request.sequence, request.controller)) {
        consecutivePollFailures += 1
        const mapped = asAdminError(cause, '读取登录状态失败。')
        if (mapped.retryable) {
          // Soft-fail: keep polling on a backoff.  Do NOT clobber an
          // existing error if we are already showing a more specific one.
          if (error.value === null) {
            error.value = {
              ...mapped,
              message: `连接 Relay 暂时失败，正在重试…（${consecutivePollFailures}）`,
            }
          }
          if (!TERMINAL_LOGIN_STATES.has(session.value?.state ?? '')) {
            schedulePoll(expectedLifecycleEpoch)
          }
        } else {
          failNonRetryable(expectedLifecycleEpoch, mapped)
        }
      }
    } finally {
      isPolling = false
      if (controller === request.controller) controller = null
    }
  }

  async function start(): Promise<void> {
    if (inFlight.value) return
    const expectedLifecycleEpoch = ++lifecycleEpoch
    stopPolling()
    stopExpiryTimer()
    invalidateRequest()
    error.value = null
    session.value = null
    consecutivePollFailures = 0
    loginOpen.value = true
    inFlight.value = true

    const input: StartWechatLoginInput = { forceFresh: false }
    const request = beginRequest()
    try {
      const next = await command.startWechatLogin(input, { signal: request.controller.signal })
      if (!isCurrent(expectedLifecycleEpoch, request.sequence, request.controller)) {
        cancelLateLogin(next.loginId)
        return
      }
      session.value = retainSafeSession(next)
      nowMs.value = Date.now()
      if (TERMINAL_LOGIN_STATES.has(next.state)) {
        verifyCode.value = ''
      } else {
        schedulePoll(expectedLifecycleEpoch)
      }
    } catch (cause) {
      if (isCurrent(expectedLifecycleEpoch, request.sequence, request.controller)) {
        error.value = asAdminError(cause, '获取登录会话失败。')
      }
    } finally {
      if (controller === request.controller) controller = null
      if (expectedLifecycleEpoch === lifecycleEpoch) inFlight.value = false
    }
  }

  async function verify(): Promise<void> {
    const current = session.value
    const code = verifyCode.value
    verifyCode.value = ''
    if (
      !current ||
      TERMINAL_LOGIN_STATES.has(current.state) ||
      !/^\d{1,16}$/.test(code) ||
      inFlight.value
    ) {
      return
    }
    const expectedLifecycleEpoch = lifecycleEpoch
    inFlight.value = true
    error.value = null
    // A verify is a state transition.  It invalidates/aborts an outstanding
    // poll before sending the code, so an older read cannot overwrite it.
    const request = beginRequest()
    try {
      const next = await command.verifyWechatLogin(
        { loginId: current.loginId, code },
        { signal: request.controller.signal },
      )
      if (!isCurrent(expectedLifecycleEpoch, request.sequence, request.controller)) return
      session.value = retainSafeSession(next)
      nowMs.value = Date.now()
      if (TERMINAL_LOGIN_STATES.has(next.state)) {
        stopPolling()
        stopExpiryTimer()
        verifyCode.value = ''
      } else {
        schedulePoll(expectedLifecycleEpoch)
      }
    } catch (cause) {
      if (isCurrent(expectedLifecycleEpoch, request.sequence, request.controller)) {
        error.value = asAdminError(cause, '提交验证码失败。')
      }
    } finally {
      if (controller === request.controller) controller = null
      if (expectedLifecycleEpoch === lifecycleEpoch) inFlight.value = false
    }
  }

  async function cancelAndClose(): Promise<void> {
    const active = session.value
    ++lifecycleEpoch
    stopPolling()
    stopExpiryTimer()
    invalidateRequest()
    consecutivePollFailures = 0
    // A request's finally block deliberately cannot update a newer lifecycle.
    // Release this lifecycle synchronously so a newly initiated Admin login may open
    // before an aborted start/verify promise eventually settles.
    inFlight.value = false
    loginOpen.value = false
    clearSensitiveState()
    error.value = null
    if (!active || TERMINAL_LOGIN_STATES.has(active.state)) return
    try {
      await command.cancelWechatLogin(active.loginId)
    } catch {
      // Closing must not retain QR data just because the best-effort
      // server-side cancellation races a terminal state.
    }
  }

  function closeTerminal(): void {
    ++lifecycleEpoch
    stopPolling()
    stopExpiryTimer()
    invalidateRequest()
    consecutivePollFailures = 0
    inFlight.value = false
    loginOpen.value = false
    clearSensitiveState()
    error.value = null
  }

  onBeforeUnmount(() => {
    void cancelAndClose()
  })

  return {
    loginOpen,
    session,
    error,
    inFlight,
    state,
    isTerminal,
    secondsLeft,
    verifyCode,
    start,
    verify,
    cancelAndClose,
    closeTerminal,
  }
}
