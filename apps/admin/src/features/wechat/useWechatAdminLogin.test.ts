import { flushPromises, mount } from '@vue/test-utils'
import { describe, expect, it, vi, afterEach } from 'vitest'
import { defineComponent } from 'vue'

import { AdminRepositoryError } from '@/contracts/error'
import type { AdminCommandRepository } from '@/data/admin-command-repository'
import type { WechatLoginSession } from '@/contracts/wechat'
import { useWechatAdminLogin } from './useWechatAdminLogin'

const waiting = (expiresAt = Date.now() + 60_000): WechatLoginSession => ({
  loginId: 'login-1',
  state: 'waiting_scan',
  qrContent: 'sensitive-qr-content',
  expiresAt,
  canSubmitVerifyCode: false,
})

function mountFlow(command: Partial<AdminCommandRepository>) {
  let flow!: ReturnType<typeof useWechatAdminLogin>
  const Host = defineComponent({
    setup() {
      flow = useWechatAdminLogin(command as AdminCommandRepository)
      return () => null
    },
  })
  const wrapper = mount(Host)
  return { wrapper, flow }
}

describe('useWechatAdminLogin', () => {
  afterEach(() => vi.useRealTimers())

  it('starts directly from Admin and polls on a bounded timer', async () => {
    vi.useFakeTimers()
    const getWechatLogin = vi.fn().mockResolvedValue({ ...waiting(), state: 'confirmed' })
    const startWechatLogin = vi.fn().mockResolvedValue(waiting())
    const { wrapper, flow } = mountFlow({ startWechatLogin, getWechatLogin })
    await flow.start()
    expect(startWechatLogin).toHaveBeenCalledWith({ forceFresh: false }, expect.any(Object))
    expect(flow.session.value?.state).toBe('waiting_scan')

    await vi.advanceTimersByTimeAsync(1_000)
    await flushPromises()
    expect(getWechatLogin).toHaveBeenCalledTimes(1)
    expect(flow.session.value?.state).toBe('confirmed')

    wrapper.unmount()
  })

  it('cancels a late server session when the start request resolves after close', async () => {
    vi.useFakeTimers()
    let resolveStart: ((value: WechatLoginSession) => void) | undefined
    const startWechatLogin = vi.fn(
      () => new Promise<WechatLoginSession>((resolve) => (resolveStart = resolve)),
    )
    const cancelWechatLogin = vi.fn().mockResolvedValue({
      loginId: 'login-1',
      state: 'cancelled',
      expiresAt: Date.now(),
      canSubmitVerifyCode: false,
    })
    const { wrapper, flow } = mountFlow({ startWechatLogin, cancelWechatLogin })
    const opening = flow.start()
    await flushPromises()
    await flow.cancelAndClose()
    resolveStart?.(waiting())
    await opening
    await flushPromises()

    expect(flow.loginOpen.value).toBe(false)
    expect(flow.session.value).toBeNull()
    expect(flow.verifyCode.value).toBe('')
    expect(cancelWechatLogin).toHaveBeenCalledWith('login-1')
    wrapper.unmount()
  })

  it('allows a new Admin session to start while a closed start request settles late', async () => {
    vi.useFakeTimers()
    let resolveStart: ((value: WechatLoginSession) => void) | undefined
    const startWechatLogin = vi
      .fn()
      .mockImplementationOnce(
        () => new Promise<WechatLoginSession>((resolve) => (resolveStart = resolve)),
      )
      .mockResolvedValueOnce(waiting())
    const cancelWechatLogin = vi.fn().mockResolvedValue({
      loginId: 'login-1',
      state: 'cancelled',
      expiresAt: Date.now(),
      canSubmitVerifyCode: false,
    })
    const { wrapper, flow } = mountFlow({ startWechatLogin, cancelWechatLogin })
    const opening = flow.start()
    await flushPromises()
    await flow.cancelAndClose()
    expect(flow.inFlight.value).toBe(false)

    resolveStart?.(waiting())
    await opening
    await flushPromises()
    await flow.start()

    expect(startWechatLogin).toHaveBeenCalledTimes(2)
    expect(startWechatLogin).toHaveBeenLastCalledWith({ forceFresh: false }, expect.any(Object))
    wrapper.unmount()
  })

  it('clears a submitted verification code even when the request fails', async () => {
    vi.useFakeTimers()
    const startWechatLogin = vi.fn().mockResolvedValue({
      ...waiting(),
      state: 'verify_code_required',
      canSubmitVerifyCode: true,
    })
    const verifyWechatLogin = vi.fn().mockRejectedValue(new Error('network'))
    const { wrapper, flow } = mountFlow({ startWechatLogin, verifyWechatLogin })
    await flow.start()
    flow.verifyCode.value = '123456'

    await flow.verify()
    expect(verifyWechatLogin).toHaveBeenCalledWith(
      { loginId: 'login-1', code: '123456' },
      expect.any(Object),
    )
    expect(flow.verifyCode.value).toBe('')
    wrapper.unmount()
  })

  it.each(['expired', 'failed'] as const)(
    'stops polling and discards QR and verify memory for the %s terminal response',
    async (state) => {
      vi.useFakeTimers()
      const getWechatLogin = vi.fn().mockResolvedValue({ ...waiting(), state })
      const startWechatLogin = vi.fn().mockResolvedValue(waiting())
      const { wrapper, flow } = mountFlow({ startWechatLogin, getWechatLogin })
      await flow.start()
      flow.verifyCode.value = '123456'
      await vi.advanceTimersByTimeAsync(1_000)
      await flushPromises()
      expect(flow.isTerminal.value).toBe(true)
      expect(flow.session.value?.qrContent).toBeUndefined()
      expect(flow.verifyCode.value).toBe('')

      await vi.advanceTimersByTimeAsync(5_000)
      expect(getWechatLogin).toHaveBeenCalledTimes(1)
      wrapper.unmount()
    },
  )

  it('invalidates an in-flight poll before verify and rejects its late non-terminal result', async () => {
    vi.useFakeTimers()
    let resolvePoll: ((value: WechatLoginSession) => void) | undefined
    let resolveVerify: ((value: WechatLoginSession) => void) | undefined
    let pollSignal: AbortSignal | undefined
    const startWechatLogin = vi.fn().mockResolvedValue({
      ...waiting(),
      state: 'verify_code_required',
      canSubmitVerifyCode: true,
    })
    const getWechatLogin = vi.fn(
      (_loginId: string, options?: { signal?: AbortSignal }) =>
        new Promise<WechatLoginSession>((resolve) => {
          pollSignal = options?.signal
          resolvePoll = resolve
        }),
    )
    const verifyWechatLogin = vi.fn(
      () => new Promise<WechatLoginSession>((resolve) => (resolveVerify = resolve)),
    )
    const { wrapper, flow } = mountFlow({ startWechatLogin, getWechatLogin, verifyWechatLogin })
    await flow.start()

    await vi.advanceTimersByTimeAsync(1_000)
    await flushPromises()
    expect(getWechatLogin).toHaveBeenCalledTimes(1)

    flow.verifyCode.value = '123456'
    const verifying = flow.verify()
    expect(pollSignal?.aborted).toBe(true)
    resolveVerify?.({
      loginId: 'login-1',
      state: 'confirmed',
      expiresAt: Date.now(),
      canSubmitVerifyCode: false,
    })
    await verifying

    resolvePoll?.(waiting())
    await flushPromises()
    expect(flow.session.value?.state).toBe('confirmed')
    expect(flow.session.value?.qrContent).toBeUndefined()
    wrapper.unmount()
  })

  it('synchronously clears inFlight when closing an in-flight verification', async () => {
    vi.useFakeTimers()
    let resolveVerify: ((value: WechatLoginSession) => void) | undefined
    const startWechatLogin = vi.fn().mockResolvedValue({
      ...waiting(),
      state: 'verify_code_required',
      canSubmitVerifyCode: true,
    })
    const verifyWechatLogin = vi.fn(
      () => new Promise<WechatLoginSession>((resolve) => (resolveVerify = resolve)),
    )
    const cancelWechatLogin = vi.fn().mockResolvedValue({
      loginId: 'login-1',
      state: 'cancelled',
      expiresAt: Date.now(),
      canSubmitVerifyCode: false,
    })
    const { wrapper, flow } = mountFlow({
      startWechatLogin,
      verifyWechatLogin,
      cancelWechatLogin,
    })
    await flow.start()
    flow.verifyCode.value = '123456'

    const verifying = flow.verify()
    expect(flow.inFlight.value).toBe(true)
    await flow.cancelAndClose()
    expect(flow.inFlight.value).toBe(false)

    resolveVerify?.({
      loginId: 'login-1',
      state: 'confirmed',
      expiresAt: Date.now(),
      canSubmitVerifyCode: false,
    })
    await verifying
    expect(flow.inFlight.value).toBe(false)
    wrapper.unmount()
  })

  it('recovers from a transient poll failure and continues to confirmation', async () => {
    vi.useFakeTimers()
    let pollCalls = 0
    const getWechatLogin = vi.fn(() => {
      pollCalls += 1
      if (pollCalls === 1) {
        // Simulate a single network blip on the first poll.
        return Promise.reject(
          new AdminRepositoryError({
            code: 'RELAY_UNAVAILABLE',
            message: 'net',
            retryable: true,
            requestId: 'req-retry',
          }),
        )
      }
      return Promise.resolve({
        ...waiting(),
        state: 'confirmed' as const,
      })
    })
    const startWechatLogin = vi.fn().mockResolvedValue(waiting())
    const { wrapper, flow } = mountFlow({ startWechatLogin, getWechatLogin })
    await flow.start()
    expect(flow.session.value?.state).toBe('waiting_scan')

    // First poll: fails with retryable error.  The flow must NOT mark the
    // session as terminal and must NOT clear the QR code.
    await vi.advanceTimersByTimeAsync(1_000)
    await flushPromises()
    expect(flow.session.value?.state).toBe('waiting_scan')
    expect(flow.session.value?.qrContent).toBe('sensitive-qr-content')
    expect(getWechatLogin).toHaveBeenCalledTimes(1)

    // The first retry is delayed by 1 second (then 2s, 4s, and 5s).
    await vi.advanceTimersByTimeAsync(1_000)
    await flushPromises()
    expect(getWechatLogin).toHaveBeenCalledTimes(2)
    expect(flow.session.value?.state).toBe('confirmed')
    expect(flow.error.value).toBeNull()
    wrapper.unmount()
  })

  it('stops immediately and clears QR memory for a non-retryable poll error', async () => {
    vi.useFakeTimers()
    const getWechatLogin = vi.fn().mockRejectedValue(
      new AdminRepositoryError({
        code: 'INTERNAL',
        message: 'permission revoked',
        retryable: false,
        requestId: 'req-terminal',
      }),
    )
    const cancelWechatLogin = vi.fn().mockResolvedValue({
      loginId: 'login-1',
      state: 'cancelled',
      expiresAt: Date.now(),
      canSubmitVerifyCode: false,
    })
    const startWechatLogin = vi.fn().mockResolvedValue(waiting())
    const { wrapper, flow } = mountFlow({ startWechatLogin, getWechatLogin, cancelWechatLogin })
    await flow.start()
    await vi.advanceTimersByTimeAsync(1_000)
    await flushPromises()
    expect(getWechatLogin).toHaveBeenCalledTimes(1)
    expect(flow.session.value?.state).toBe('failed')
    expect(flow.session.value?.qrContent).toBeUndefined()
    expect(flow.verifyCode.value).toBe('')
    expect(cancelWechatLogin).toHaveBeenCalledWith('login-1')
    await vi.advanceTimersByTimeAsync(20_000)
    await flushPromises()
    expect(getWechatLogin).toHaveBeenCalledTimes(1)
    wrapper.unmount()
  })

  it('expires locally after retryable network failures, clears sensitive data, and retires Relay login', async () => {
    vi.useFakeTimers()
    const expiresAt = Date.now() + 3_000
    const getWechatLogin = vi.fn().mockRejectedValue(
      new AdminRepositoryError({
        code: 'RELAY_UNAVAILABLE',
        message: 'offline',
        retryable: true,
        requestId: 'req-offline',
      }),
    )
    const cancelWechatLogin = vi.fn().mockResolvedValue({
      loginId: 'login-1',
      state: 'cancelled',
      expiresAt,
      canSubmitVerifyCode: false,
    })
    const startWechatLogin = vi.fn().mockResolvedValue(waiting(expiresAt))
    const { wrapper, flow } = mountFlow({ startWechatLogin, getWechatLogin, cancelWechatLogin })

    await flow.start()
    flow.verifyCode.value = '123456'
    await vi.advanceTimersByTimeAsync(1_000)
    await flushPromises()
    expect(getWechatLogin).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(1_000)
    await flushPromises()
    expect(getWechatLogin).toHaveBeenCalledTimes(2)
    await vi.advanceTimersByTimeAsync(1_000)
    await flushPromises()

    expect(flow.state.value).toBe('expired')
    expect(flow.session.value?.qrContent).toBeUndefined()
    expect(flow.verifyCode.value).toBe('')
    expect(cancelWechatLogin).toHaveBeenCalledWith('login-1')
    await vi.advanceTimersByTimeAsync(20_000)
    await flushPromises()
    expect(getWechatLogin).toHaveBeenCalledTimes(2)
    wrapper.unmount()
  })

  it('clears browser state synchronously when best-effort cancellation does not settle', async () => {
    vi.useFakeTimers()
    const startWechatLogin = vi.fn().mockResolvedValue(waiting())
    const cancelWechatLogin = vi.fn(() => new Promise<WechatLoginSession>(() => undefined))
    const { wrapper, flow } = mountFlow({ startWechatLogin, cancelWechatLogin })
    await flow.start()
    flow.verifyCode.value = '123456'

    void flow.cancelAndClose()
    expect(flow.loginOpen.value).toBe(false)
    expect(flow.session.value).toBeNull()
    expect(flow.verifyCode.value).toBe('')
    expect(cancelWechatLogin).toHaveBeenCalledWith('login-1')
    wrapper.unmount()
  })

  it('retires the active Admin login on unmount', async () => {
    vi.useFakeTimers()
    const cancelWechatLogin = vi.fn().mockResolvedValue({
      loginId: 'login-1',
      state: 'cancelled',
      expiresAt: Date.now(),
      canSubmitVerifyCode: false,
    })
    const { wrapper, flow } = mountFlow({
      startWechatLogin: vi.fn().mockResolvedValue(waiting()),
      cancelWechatLogin,
    })
    await flow.start()
    wrapper.unmount()
    await flushPromises()

    expect(cancelWechatLogin).toHaveBeenCalledWith('login-1')
    expect(flow.session.value).toBeNull()
    expect(flow.verifyCode.value).toBe('')
  })
})
