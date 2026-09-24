import { computed, ref } from 'vue'

import { useAsyncResource } from '@/composables/useAsyncResource'
import { useAdminCapabilities } from '@/composables/useAdminCapabilities'
import { useAdminRepository } from '@/composables/useAdminRepository'
import { toAdminError, type AdminError } from '@/contracts/error'
import type { ChannelEventItem, WechatTestReceipt } from '@/contracts/wechat'

import { useWechatAdminLogin } from './useWechatAdminLogin'
import { wechatStateVisual } from './wechat-status'

const TEST_RECEIPT_TEXT: Record<WechatTestReceipt['state'], string> = {
  accepted_by_relay: 'Relay 已接管',
  provider_accepted: '微信服务已接受',
  phone_displayed_unknown: '手机是否展示需人工确认',
}

/** Coordinates page data and commands; QR/verify lifetime remains in useWechatAdminLogin. */
export function useWechatController() {
  const adminRepository = useAdminRepository()
  const { can: canCapability, capabilityReason } = useAdminCapabilities()

  const wechatResource = useAsyncResource({
    fetcher: (signal?: AbortSignal) => adminRepository.read.getWechatStatus({ signal }),
  })
  const eventsResource = useAsyncResource({
    fetcher: (signal?: AbortSignal) =>
      adminRepository.read.listChannelEvents({ limit: 20 }, { signal }),
  })

  const loading = computed(() => wechatResource.loading.value)
  const refreshing = computed(() => wechatResource.refreshing.value)
  const wechatError = computed(() => wechatResource.error.value)
  const wechat = computed(() => wechatResource.data.value)
  const events = computed<ChannelEventItem[]>(() => eventsResource.data.value?.items ?? [])
  const eventsLoading = computed(() => eventsResource.loading.value)
  const statusVisual = computed(() => (wechat.value ? wechatStateVisual(wechat.value.state) : null))
  const canStartLogin = computed(() => canCapability('admin_wechat_login_v2'))
  const canManageWechat = computed(() => canCapability('admin_wechat_manage_v2'))

  const login = useWechatAdminLogin(adminRepository.command)

  const testReceipt = ref<WechatTestReceipt | null>(null)
  const testError = ref<AdminError | null>(null)
  const testInFlight = ref(false)
  const disconnectOpen = ref(false)
  const disconnectInFlight = ref(false)
  const disconnectError = ref<AdminError | null>(null)

  async function refresh(): Promise<void> {
    await Promise.allSettled([wechatResource.refresh(), eventsResource.refresh()])
  }

  async function sendTest(): Promise<void> {
    if (!canManageWechat.value || testInFlight.value) return
    testInFlight.value = true
    testError.value = null
    testReceipt.value = null
    try {
      testReceipt.value = await adminRepository.command.sendWechatTest()
      // Relay acknowledgement is not provider delivery. Refresh only the
      // observable relay status and queue summary after accepting the command.
      await refresh()
    } catch (err) {
      testError.value = toAdminError(err, {
        code: 'INTERNAL',
        message: '发送测试失败，请重试。',
        retryable: true,
        requestId: null,
      })
    } finally {
      testInFlight.value = false
    }
  }

  async function openLogin(): Promise<void> {
    if (!canStartLogin.value) return
    await login.start()
  }

  async function closeLogin(): Promise<void> {
    if (login.isTerminal.value) {
      login.closeTerminal()
      return
    }
    await login.cancelAndClose()
  }

  async function submitVerify(): Promise<void> {
    await login.verify()
  }

  function openDisconnect(): void {
    if (!canManageWechat.value) return
    disconnectError.value = null
    disconnectOpen.value = true
  }

  function closeDisconnect(): void {
    if (disconnectInFlight.value) return
    disconnectError.value = null
    disconnectOpen.value = false
  }

  async function confirmDisconnect(): Promise<void> {
    if (!canManageWechat.value || disconnectInFlight.value) return
    disconnectInFlight.value = true
    disconnectError.value = null
    try {
      await adminRepository.command.disconnectWechat({ reason: 'operator request' })
      disconnectOpen.value = false
      await refresh()
    } catch (err) {
      disconnectError.value = toAdminError(err, {
        code: 'INTERNAL',
        message: '断开连接失败，请重试。',
        retryable: true,
        requestId: null,
      })
    } finally {
      disconnectInFlight.value = false
    }
  }

  return {
    loading,
    refreshing,
    wechatError,
    wechat,
    events,
    eventsLoading,
    statusVisual,
    capabilityReason,
    canStartLogin,
    canManageWechat,
    login,
    testReceipt,
    testError,
    testInFlight,
    disconnectOpen,
    disconnectInFlight,
    disconnectError,
    testReceiptText: TEST_RECEIPT_TEXT,
    refresh,
    sendTest,
    openLogin,
    closeLogin,
    submitVerify,
    openDisconnect,
    closeDisconnect,
    confirmDisconnect,
  }
}
