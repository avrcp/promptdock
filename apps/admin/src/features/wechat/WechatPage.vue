<script setup lang="ts">
import { computed } from 'vue'
import { RefreshCw } from 'lucide-vue-next'

import AppButton from '@/components/AppButton.vue'
import AdminErrorAlert from '@/components/AdminErrorAlert.vue'
import SkeletonBlock from '@/components/SkeletonBlock.vue'

import WechatActions from './WechatActions.vue'
import WechatEventTable from './WechatEventTable.vue'
import WechatLoginDialog from './WechatLoginDialog.vue'
import WechatStatusCard from './WechatStatusCard.vue'
import { useWechatController } from './useWechatController'

const {
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
  testReceiptText,
  refresh,
  sendTest,
  openLogin,
  closeLogin,
  submitVerify,
  openDisconnect,
  closeDisconnect,
  confirmDisconnect,
} = useWechatController()

const loginOpen = computed(() => login.loginOpen.value)
const loginSession = computed(() => login.session.value)
const loginState = computed(() => login.state.value)
const loginError = computed(() => login.error.value)
const loginInFlight = computed(() => login.inFlight.value)
const loginSecondsLeft = computed(() => login.secondsLeft.value)
const loginVerifyCode = computed({
  get: () => login.verifyCode.value,
  set: (value: string) => {
    login.verifyCode.value = value
  },
})
</script>

<template>
  <section>
    <div
      v-if="capabilityReason('admin_wechat_manage_v2') || capabilityReason('admin_wechat_login_v2')"
      class="wechat__capability-hints"
      role="status"
      data-testid="wechat-capability-reason"
    >
      <p v-if="capabilityReason('admin_wechat_login_v2')">
        扫码登录：{{ capabilityReason('admin_wechat_login_v2') }}
      </p>
      <p v-if="capabilityReason('admin_wechat_manage_v2')">
        微信通道管理：{{ capabilityReason('admin_wechat_manage_v2') }}
      </p>
    </div>

    <AdminErrorAlert v-if="wechatError" :error="wechatError" @retry="refresh" />

    <div
      v-if="loading"
      class="wechat__skeleton"
      data-testid="wechat-skeleton"
      role="status"
      aria-live="polite"
    >
      <span class="sr-only">正在加载微信通道</span>
      <SkeletonBlock v-for="i in 4" :key="i" variant="metric" />
    </div>

    <template v-else-if="wechat && statusVisual">
      <div class="wechat__workspace">
        <WechatStatusCard :wechat="wechat" :status-visual="statusVisual">
          <template #header-actions>
            <AppButton variant="secondary" size="sm" :loading="refreshing" @click="refresh">
              <template #default>
                <RefreshCw :size="14" aria-hidden="true" />
                <span>刷新</span>
              </template>
            </AppButton>
          </template>
          <WechatActions
            :channel-state="wechat.state"
            :can-start-login="canStartLogin"
            :can-manage-wechat="canManageWechat"
            :login-open="loginOpen"
            :login-in-flight="loginInFlight"
            :login-capability-reason="capabilityReason('admin_wechat_login_v2')"
            :management-capability-reason="capabilityReason('admin_wechat_manage_v2')"
            :test-receipt="testReceipt"
            :test-receipt-text="testReceiptText"
            :test-error="testError"
            :test-in-flight="testInFlight"
            :disconnect-open="disconnectOpen"
            :disconnect-in-flight="disconnectInFlight"
            :disconnect-error="disconnectError"
            @start-login="openLogin"
            @send-test="sendTest"
            @open-disconnect="openDisconnect"
            @confirm-disconnect="confirmDisconnect"
            @close-disconnect="closeDisconnect"
          />
        </WechatStatusCard>
      </div>

      <WechatEventTable :events="events" :loading="eventsLoading" />
    </template>

    <WechatLoginDialog
      :open="loginOpen"
      :session="loginSession"
      :state="loginState"
      :error="loginError"
      :in-flight="loginInFlight"
      :seconds-left="loginSecondsLeft"
      :verify-code="loginVerifyCode"
      :can-start-login="canStartLogin"
      @close="closeLogin"
      @start="openLogin"
      @verify="submitVerify"
      @update:verify-code="(value: string) => (loginVerifyCode = value)"
    />
  </section>
</template>

<style scoped>
.wechat__workspace {
  display: grid;
  gap: var(--pd-space-16);
}

.wechat__skeleton {
  display: grid;
  gap: var(--pd-space-12);
}

.wechat__capability-hints {
  display: grid;
  gap: var(--pd-space-4);
  margin-bottom: var(--pd-space-16);
  padding: var(--pd-space-12) var(--pd-space-16);
  border: 1px solid var(--pd-feedback-warning-muted);
  border-inline-start-color: var(--pd-feedback-warning);
  border-radius: var(--pd-radius-sm);
  background: var(--pd-feedback-warning-subtle);
  color: var(--pd-text-default);
  font-size: var(--pd-font-size-13);
}

.wechat__capability-hints p {
  margin: 0;
}
</style>
