<script setup lang="ts">
import { computed, nextTick, ref, useId, watch } from 'vue'
import { X } from 'lucide-vue-next'

import AppButton from './AppButton.vue'
import { useOverlayScrollLock } from '@/composables/useOverlayScrollLock'
import { FOCUSABLE_SELECTOR, focusPreferredOrFirst } from '@/utils/focus-management'

interface CredentialReceiptModel {
  receiptId: string
  action: 'create' | 'rotate'
  deviceId: string
  oneTimeToken: string
  issuedAt: number
}

interface CredentialReceiptDialogProps {
  open: boolean
  receipt: CredentialReceiptModel | null
  deviceName: string
  actionLabel: string
  actionHint: string
  pending?: boolean
  closable?: boolean
}

const props = withDefaults(defineProps<CredentialReceiptDialogProps>(), {
  actionLabel: '新建设备',
  actionHint: '关闭后无法再次查看，请立即保存。',
  pending: false,
  closable: true,
})

const emit = defineEmits<{
  (e: 'update:open', value: boolean): void
  (e: 'close'): void
}>()

const dialogRef = ref<HTMLDivElement | null>(null)
const closeBtnRef = ref<HTMLButtonElement | null>(null)
const confirmRef = ref<HTMLButtonElement | null>(null)
const returnFocus = ref<HTMLElement | null>(null)
const titleId = useId()
const hintId = useId()
const copyState = ref<'idle' | 'copied' | 'failed'>('idle')
const clearClipboardState = ref<'idle' | 'cleared' | 'failed'>('idle')
const { lock, unlock } = useOverlayScrollLock()

const isOpen = computed(() => props.open && props.receipt !== null)
const canClose = computed(() => props.closable && !props.pending)

const issuedAtLabel = computed(() => {
  if (!props.receipt) return ''
  try {
    return new Date(props.receipt.issuedAt).toISOString()
  } catch {
    return String(props.receipt.issuedAt)
  }
})

function requestClose(): void {
  if (!canClose.value) return
  emit('update:open', false)
  emit('close')
}

function focusables(): HTMLElement[] {
  if (!dialogRef.value) return []
  return Array.from(dialogRef.value.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR))
}

function onKeydown(event: KeyboardEvent): void {
  if (!isOpen.value) return
  if (event.key === 'Escape' && canClose.value) {
    event.preventDefault()
    requestClose()
    return
  }
  if (event.key !== 'Tab') return
  const focusList = focusables()
  if (focusList.length === 0) {
    event.preventDefault()
    dialogRef.value?.focus()
    return
  }
  const first = focusList[0]
  const last = focusList[focusList.length - 1]
  if (!first || !last) return
  const active = document.activeElement as HTMLElement | null
  if (event.shiftKey) {
    if (active === first || !dialogRef.value?.contains(active)) {
      event.preventDefault()
      last.focus()
    }
  } else {
    if (active === last) {
      event.preventDefault()
      first.focus()
    }
  }
}

async function copyToken(): Promise<void> {
  if (!props.receipt) return
  if (typeof navigator === 'undefined' || !navigator.clipboard) {
    copyState.value = 'failed'
    return
  }
  try {
    await navigator.clipboard.writeText(props.receipt.oneTimeToken)
    copyState.value = 'copied'
  } catch {
    copyState.value = 'failed'
  }
}

async function clearClipboard(): Promise<void> {
  if (typeof navigator === 'undefined' || !navigator.clipboard) {
    clearClipboardState.value = 'failed'
    return
  }
  try {
    // Clipboard ownership belongs to the browser and operating system. This
    // only attempts to replace the current text; it cannot revoke other apps'
    // history, sync, or prior copies.
    await navigator.clipboard.writeText('')
    clearClipboardState.value = 'cleared'
  } catch {
    clearClipboardState.value = 'failed'
  }
}

function resolveEl(ref: HTMLElement | { $el?: HTMLElement } | null): HTMLElement | null {
  if (!ref) return null
  if (ref instanceof HTMLElement) return ref
  const el = (ref as { $el?: HTMLElement }).$el
  return el instanceof HTMLElement ? el : null
}

watch(
  isOpen,
  async (value) => {
    copyState.value = 'idle'
    clearClipboardState.value = 'idle'
    if (value) {
      if (typeof document !== 'undefined') {
        returnFocus.value = (document.activeElement as HTMLElement | null) ?? null
        lock()
      }
      await nextTick()
      focusPreferredOrFirst(dialogRef.value, resolveEl(confirmRef.value))
    } else {
      if (typeof document !== 'undefined') {
        unlock()
        if (returnFocus.value && document.contains(returnFocus.value)) {
          returnFocus.value.focus()
        }
        returnFocus.value = null
      }
    }
  },
  { immediate: true },
)
</script>

<template>
  <Teleport to="body">
    <div
      v-if="isOpen && receipt"
      class="receipt__backdrop"
      :aria-busy="pending || undefined"
      @mousedown.self="requestClose"
      data-testid="credential-receipt"
    >
      <div
        ref="dialogRef"
        class="receipt"
        role="dialog"
        aria-modal="true"
        :aria-labelledby="titleId"
        :aria-describedby="hintId"
        tabindex="-1"
        @keydown="onKeydown"
      >
        <header class="receipt__header">
          <div>
            <h2 :id="titleId" class="receipt__title">{{ actionLabel }} · 一次性凭证</h2>
            <p class="receipt__sub">设备：{{ deviceName }}</p>
          </div>
          <button
            ref="closeBtnRef"
            type="button"
            class="receipt__close"
            aria-label="关闭一次性凭证对话框"
            :disabled="!canClose"
            @click="requestClose"
          >
            <X :size="16" aria-hidden="true" />
          </button>
        </header>
        <div class="receipt__body">
          <p :id="hintId" class="receipt__warning" role="note">
            {{ actionHint }}
          </p>
          <dl class="receipt__meta">
            <div class="receipt__meta-row">
              <dt>凭证 ID</dt>
              <dd class="mono break-anywhere">{{ receipt.receiptId }}</dd>
            </div>
            <div class="receipt__meta-row">
              <dt>设备 ID</dt>
              <dd class="mono break-anywhere">{{ receipt.deviceId }}</dd>
            </div>
            <div class="receipt__meta-row">
              <dt>签发时间</dt>
              <dd class="tabular">{{ issuedAtLabel }}</dd>
            </div>
          </dl>
          <div class="receipt__token">
            <label for="credential-receipt-token" class="receipt__token-label">
              一次性 Token（仅本次显示）
            </label>
            <textarea
              id="credential-receipt-token"
              class="receipt__token-value"
              readonly
              rows="3"
              :value="receipt.oneTimeToken"
              data-testid="credential-receipt-token"
            />
            <div class="receipt__token-actions">
              <AppButton size="sm" @click="copyToken">
                <span>{{ copyState === 'copied' ? '已复制' : '复制 Token' }}</span>
              </AppButton>
              <AppButton variant="secondary" size="sm" @click="clearClipboard">
                <span>尝试清空剪贴板</span>
              </AppButton>
              <span
                v-if="copyState === 'copied'"
                class="receipt__copy-feedback"
                role="status"
                aria-live="polite"
              >
                已复制到剪贴板
              </span>
              <span
                v-else-if="copyState === 'failed'"
                class="receipt__copy-feedback receipt__copy-feedback--error"
                role="status"
                aria-live="polite"
              >
                复制失败，请手动选择复制
              </span>
              <span
                v-else-if="clearClipboardState === 'cleared'"
                class="receipt__copy-feedback"
                role="status"
                aria-live="polite"
              >
                已尝试用空文本替换剪贴板内容
              </span>
              <span
                v-else-if="clearClipboardState === 'failed'"
                class="receipt__copy-feedback receipt__copy-feedback--error"
                role="status"
                aria-live="polite"
              >
                无法清空剪贴板，请按系统方式自行处理
              </span>
            </div>
            <p class="receipt__clipboard-risk" role="note">
              复制后内容会进入系统剪贴板；Admin
              无法控制其他应用的读取、历史记录或同步。清空操作仅为最佳努力，并不保证彻底移除。
            </p>
          </div>
        </div>
        <footer class="receipt__footer">
          <AppButton ref="confirmRef" variant="primary" :disabled="!canClose" @click="requestClose">
            我已保存，关闭
          </AppButton>
        </footer>
      </div>
    </div>
  </Teleport>
</template>

<style scoped>
.receipt__backdrop {
  position: fixed;
  inset: 0;
  background: var(--pd-overlay-bg);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: var(--pd-z-overlay);
  padding: var(--pd-space-16);
}

.receipt {
  background: var(--pd-container-workspace-bg);
  color: var(--pd-text-default);
  border: 1px solid var(--pd-border-overlay);
  border-radius: var(--pd-radius-lg);
  box-shadow: var(--pd-shadow-lg);
  max-width: var(--pd-dialog-max-width);
  width: 100%;
  max-height: 90dvh;
  display: flex;
  flex-direction: column;
  outline: none;
}

.receipt__header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--pd-space-12);
  padding: var(--pd-space-16);
  border-bottom: 1px solid var(--pd-border-separator);
}

.receipt__title {
  font-size: var(--pd-font-size-14);
  font-weight: var(--pd-font-weight-semibold);
  line-height: var(--pd-line-height-compact);
}

.receipt__sub {
  margin-top: var(--pd-space-4);
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-12);
  line-height: var(--pd-line-height-metadata);
}

.receipt__close {
  position: relative;
  width: var(--pd-icon-button-size-sm);
  height: var(--pd-icon-button-size-sm);
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: var(--pd-radius-sm);
  color: var(--pd-text-muted);
  background: transparent;
  border: 1px solid transparent;
}

/* Extend the hit area to the 40px desktop target without growing the visual button. */
.receipt__close::after {
  content: '';
  position: absolute;
  inset: -6px;
}

.receipt__close:hover:not(:disabled) {
  background: var(--pd-control-bg-hovered);
  color: var(--pd-text-default);
}

.receipt__close:disabled {
  cursor: not-allowed;
  color: var(--pd-text-disabled);
}

.receipt__body {
  padding: var(--pd-space-16);
  display: flex;
  flex-direction: column;
  gap: var(--pd-space-16);
  overflow-y: auto;
}

.receipt__warning {
  background: var(--pd-feedback-warning-subtle);
  color: var(--pd-feedback-warning);
  border: 1px solid var(--pd-feedback-warning-muted);
  border-radius: var(--pd-radius-sm);
  padding: var(--pd-space-8) var(--pd-space-12);
  font-size: var(--pd-font-size-13);
  line-height: var(--pd-line-height-ui);
}

.receipt__meta {
  display: flex;
  flex-direction: column;
  gap: var(--pd-space-8);
  background: var(--pd-container-panel-bg);
  border: 1px solid var(--pd-border-separator);
  border-radius: var(--pd-radius-sm);
  padding: var(--pd-space-12);
}

.receipt__meta-row {
  display: flex;
  justify-content: space-between;
  gap: var(--pd-space-12);
  font-size: var(--pd-font-size-13);
  line-height: var(--pd-line-height-ui);
}

.receipt__meta-row dt {
  color: var(--pd-text-muted);
}

.receipt__meta-row dd {
  color: var(--pd-text-default);
  font-weight: var(--pd-font-weight-medium);
  text-align: end;
  word-break: break-all;
}

.receipt__token {
  display: flex;
  flex-direction: column;
  gap: var(--pd-space-8);
}

.receipt__token-label {
  font-size: var(--pd-font-size-13);
  color: var(--pd-text-muted);
  font-weight: var(--pd-font-weight-medium);
}

.receipt__token-value {
  width: 100%;
  background: var(--pd-container-workspace-bg);
  color: var(--pd-text-default);
  border: 1px solid var(--pd-control-border-default);
  border-radius: var(--pd-radius-sm);
  padding: var(--pd-space-8) var(--pd-space-12);
  font-family: var(--pd-font-code);
  font-size: var(--pd-font-size-13);
  resize: none;
  line-height: var(--pd-line-height-code);
}

.receipt__token-actions {
  display: flex;
  align-items: center;
  gap: var(--pd-space-12);
  flex-wrap: wrap;
}

.receipt__copy-feedback {
  font-size: var(--pd-font-size-12);
  color: var(--pd-feedback-success);
}

.receipt__copy-feedback--error {
  color: var(--pd-feedback-danger);
}

.receipt__clipboard-risk {
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-12);
  line-height: var(--pd-line-height-reading);
}

.receipt__footer {
  padding: var(--pd-space-16);
  border-top: 1px solid var(--pd-border-separator);
  display: flex;
  justify-content: flex-end;
}

@media (max-width: 767px) {
  .receipt__footer {
    flex-direction: column;
  }
}
</style>
