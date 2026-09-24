<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'

import AppButton from '@/components/AppButton.vue'
import AppDialog from '@/components/AppDialog.vue'
import InlineAlert from '@/components/InlineAlert.vue'
import StatusChip from '@/components/StatusChip.vue'
import type { AdminError } from '@/contracts/error'
import type { WechatLoginSession, WechatLoginState } from '@/contracts/wechat'

import WechatQrCanvas from './WechatQrCanvas.vue'
import { loginStateVisual } from './wechat-status'

const props = defineProps<{
  open: boolean
  session: WechatLoginSession | null
  state: WechatLoginState
  error: AdminError | null
  inFlight: boolean
  secondsLeft: number
  verifyCode: string
  canStartLogin: boolean
}>()

const emit = defineEmits<{
  (e: 'close'): void
  (e: 'start'): void
  (e: 'verify'): void
  (e: 'update:verifyCode', value: string): void
}>()

const verifyInputRef = ref<HTMLInputElement | null>(null)
const verifyCodeModel = computed({
  get: () => props.verifyCode,
  set: (value: string) => emit('update:verifyCode', value),
})
const currentLoginVisual = computed(() => loginStateVisual(props.state))
const countdownLabel = computed(() => {
  const minutes = Math.floor(props.secondsLeft / 60)
  const seconds = props.secondsLeft % 60
  return `${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`
})
const showQr = computed(
  () =>
    Boolean(props.session?.qrContent) &&
    ['waiting_scan', 'scanned', 'verify_code_required'].includes(props.state),
)
const canSubmitVerify = computed(
  () => props.state === 'verify_code_required' && props.session?.canSubmitVerifyCode === true,
)
const canSubmitCode = computed(() => /^\d{1,16}$/.test(verifyCodeModel.value) && !props.inFlight)
const primaryLabel = computed(() =>
  props.state === 'confirmed' || props.state === 'already_connected' ? '完成' : '取消登录',
)

watch(
  () => props.state,
  (state, previous) => {
    if (state === 'verify_code_required' && previous !== 'verify_code_required') {
      void nextTick(() => verifyInputRef.value?.focus())
    }
  },
)

function onDialogOpenUpdate(open: boolean): void {
  if (!open) emit('close')
}
</script>

<template>
  <AppDialog
    :open="open"
    title="微信扫码登录"
    :primary-loading="inFlight"
    :close-on-backdrop="!inFlight"
    :close-on-escape="!inFlight"
    :close-on-secondary="false"
    :show-secondary="false"
    initial-focus="primary"
    :primary-label="primaryLabel"
    @update:open="onDialogOpenUpdate"
    @primary="emit('close')"
    @secondary="emit('close')"
  >
    <div class="wechat-login">
      <div
        v-if="inFlight || state === 'fetching_qr'"
        class="wechat-login__status"
        aria-live="polite"
      >
        获取登录会话中…
      </div>

      <template v-else>
        <div class="wechat-login__state">
          <StatusChip :tone="currentLoginVisual.tone" :label="currentLoginVisual.label" />
          <span v-if="showQr && secondsLeft > 0" class="wechat-login__countdown">
            剩余 {{ countdownLabel }}
          </span>
        </div>

        <div v-if="showQr" class="wechat-login__qr" data-testid="wechat-qr">
          <WechatQrCanvas :content="session?.qrContent ?? null" />
        </div>

        <p v-if="state === 'waiting_scan'" class="wechat-login__hint">
          请使用手机微信扫描上方二维码。
        </p>
        <p v-else-if="state === 'scanned'" class="wechat-login__hint">
          已扫码，请在手机上确认登录。
        </p>
        <p v-else-if="state === 'verify_code_required'" class="wechat-login__hint">
          需要验证码；请在下方提交手机验证码。
        </p>
        <p v-else-if="state === 'refreshing_qr'" class="wechat-login__hint">二维码刷新中…</p>
        <p
          v-else-if="state === 'confirmed' || state === 'already_connected'"
          class="wechat-login__hint wechat-login__hint--ok"
        >
          登录成功，通道已就绪。
        </p>
        <p v-else-if="state === 'cancelled'" class="wechat-login__hint">登录已取消。</p>
        <p v-else-if="state === 'expired'" class="wechat-login__hint wechat-login__hint--warn">
          二维码已过期，请重新生成。
        </p>
        <p v-else-if="state === 'failed'" class="wechat-login__hint wechat-login__hint--warn">
          登录失败：{{ session?.errorCode ?? '请检查 Relay 状态后重试。' }}
        </p>

        <div v-if="error" class="wechat-login__error">
          <InlineAlert
            tone="danger"
            title="登录失败"
            :description="error.message"
            :code="error.code"
          />
        </div>

        <form v-if="canSubmitVerify" class="wechat-login__verify" @submit.prevent="emit('verify')">
          <label for="wechat-login-verify-code">验证码</label>
          <input
            id="wechat-login-verify-code"
            ref="verifyInputRef"
            v-model="verifyCodeModel"
            data-testid="wechat-login-verify-code"
            inputmode="numeric"
            autocomplete="one-time-code"
            pattern="[0-9]*"
            maxlength="16"
            aria-describedby="wechat-login-verify-help"
          />
          <span id="wechat-login-verify-help">仅数字，最多 16 位。</span>
          <AppButton type="submit" size="sm" :disabled="!canSubmitCode">提交验证码</AppButton>
        </form>

        <div
          v-if="state === 'expired' || state === 'cancelled' || state === 'failed'"
          class="wechat-login__controls"
        >
          <AppButton
            variant="primary"
            size="sm"
            data-testid="login-regenerate"
            :disabled="!canStartLogin || inFlight"
            @click="emit('start')"
          >
            重新生成
          </AppButton>
        </div>
      </template>
    </div>
  </AppDialog>
</template>

<style scoped>
.wechat-login {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: var(--pd-space-12);
  text-align: center;
}

.wechat-login__state {
  display: flex;
  align-items: center;
  gap: var(--pd-space-12);
}

.wechat-login__countdown {
  color: var(--pd-text-muted);
  font-family: var(--pd-font-code);
  font-size: var(--pd-font-size-13);
}

.wechat-login__qr {
  display: flex;
  align-items: center;
  justify-content: center;
  padding: var(--pd-space-16);
  background: var(--pd-qr-surface-bg);
  border-radius: var(--pd-radius-sm);
}

.wechat-login__verify {
  display: grid;
  width: min(100%, 280px);
  gap: var(--pd-space-8);
  text-align: start;
}

.wechat-login__verify input {
  min-height: var(--pd-control-height-md);
  padding: 0 var(--pd-space-8);
  color: var(--pd-text-default);
  background: var(--pd-control-bg);
  border: 1px solid var(--pd-control-border-default);
  border-radius: var(--pd-radius-sm);
  font-family: var(--pd-font-code);
  font-size: var(--pd-font-size-18);
  letter-spacing: var(--pd-letter-spacing-wide);
}

.wechat-login__verify span {
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-12);
}

.wechat-login__hint {
  font-size: var(--pd-font-size-13);
  color: var(--pd-text-muted);
  margin: 0;
}

.wechat-login__hint--ok {
  color: var(--pd-feedback-success);
}

.wechat-login__hint--warn {
  color: var(--pd-feedback-warning);
}

.wechat-login__controls {
  display: flex;
  gap: var(--pd-space-8);
  flex-wrap: wrap;
  justify-content: center;
}

@media (max-width: 767px) {
  .wechat-login {
    align-items: stretch;
  }

  .wechat-login__state,
  .wechat-login__controls {
    justify-content: center;
  }

  .wechat-login__qr {
    align-self: center;
    max-width: 100%;
  }
}
</style>
