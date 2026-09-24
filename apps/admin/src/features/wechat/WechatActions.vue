<script setup lang="ts">
import { computed } from 'vue'
import { ScanLine, Smartphone, Unplug } from 'lucide-vue-next'

import AppButton from '@/components/AppButton.vue'
import AppDialog from '@/components/AppDialog.vue'
import InlineAlert from '@/components/InlineAlert.vue'
import type { AdminError } from '@/contracts/error'
import type { WechatTestReceipt } from '@/contracts/wechat'

const props = defineProps<{
  channelState: string
  canStartLogin: boolean
  canManageWechat: boolean
  loginOpen: boolean
  loginInFlight: boolean
  loginCapabilityReason: string | null
  managementCapabilityReason: string | null
  testReceipt: WechatTestReceipt | null
  testReceiptText: Record<WechatTestReceipt['state'], string>
  testError: AdminError | null
  testInFlight: boolean
  disconnectOpen: boolean
  disconnectInFlight: boolean
  disconnectError: AdminError | null
}>()

const channelReady = computed(() => props.channelState === 'ready')

const emit = defineEmits<{
  (e: 'start-login'): void
  (e: 'send-test'): void
  (e: 'open-disconnect'): void
  (e: 'confirm-disconnect'): void
  (e: 'close-disconnect'): void
}>()
</script>

<template>
  <div class="wechat__actions">
    <AppButton
      :variant="channelReady ? 'secondary' : 'primary'"
      size="sm"
      data-testid="wechat-login"
      :loading="loginInFlight"
      :disabled="loginOpen || !canStartLogin"
      :aria-describedby="loginCapabilityReason ? 'wechat-capability-reason' : undefined"
      @click="emit('start-login')"
    >
      <template #default>
        <ScanLine :size="14" aria-hidden="true" />
        <span>扫码登录</span>
      </template>
    </AppButton>
    <AppButton
      :variant="channelReady ? 'primary' : 'secondary'"
      size="sm"
      data-testid="wechat-test"
      :loading="testInFlight"
      :disabled="!canManageWechat"
      :aria-describedby="managementCapabilityReason ? 'wechat-capability-reason' : undefined"
      @click="emit('send-test')"
    >
      <template #default>
        <Smartphone :size="14" aria-hidden="true" />
        <span>发送测试</span>
      </template>
    </AppButton>
    <AppButton
      variant="danger"
      size="sm"
      data-testid="wechat-disconnect"
      :disabled="!canManageWechat"
      :aria-describedby="managementCapabilityReason ? 'wechat-capability-reason' : undefined"
      @click="emit('open-disconnect')"
    >
      <template #default>
        <Unplug :size="14" aria-hidden="true" />
        <span>断开连接</span>
      </template>
    </AppButton>
  </div>
  <p class="wechat__ownership-note">
    云端微信通道仅由 Relay Admin 管理；PromptDock Desktop 只能选择是否使用云端推送。
  </p>
  <InlineAlert
    v-if="testReceipt"
    tone="success"
    :title="testReceiptText[testReceipt.state]"
    class="wechat__test-result"
  />
  <InlineAlert
    v-if="testError"
    tone="danger"
    title="发送测试失败"
    :description="testError.message"
    :code="testError.code"
  />

  <AppDialog
    :open="disconnectOpen"
    title="断开微信通道"
    description="此操作只会断开 Relay 的微信通道，不会恢复 Desktop Local。Desktop 会观察到断开状态；如需继续使用，请重新登录。队列会保留并等待重新连接。"
    primary-label="断开连接"
    primary-variant="danger"
    :primary-loading="disconnectInFlight"
    :primary-disabled="!canManageWechat"
    :close-on-backdrop="!disconnectInFlight"
    :close-on-escape="!disconnectInFlight"
    @primary="emit('confirm-disconnect')"
    @secondary="emit('close-disconnect')"
    @update:open="(open: boolean) => !open && emit('close-disconnect')"
  >
    <InlineAlert
      v-if="disconnectError"
      tone="danger"
      title="断开连接失败"
      :description="disconnectError.message"
      :code="disconnectError.code"
    />
  </AppDialog>
</template>

<style scoped>
.wechat__actions {
  display: flex;
  gap: var(--pd-space-8);
  flex-wrap: wrap;
}

.wechat__ownership-note {
  margin: var(--pd-space-12) 0 0;
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-12);
  line-height: var(--pd-line-height-ui);
}

.wechat__test-result {
  margin-top: var(--pd-space-12);
}

@media (max-width: 767px) {
  .wechat__actions {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .wechat__actions :deep(.app-button) {
    width: 100%;
  }

  .wechat__actions :deep(.app-button:last-child) {
    grid-column: span 2;
  }
}
</style>
