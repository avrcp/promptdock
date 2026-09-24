<script setup lang="ts">
import { computed, useId } from 'vue'
import { AlertTriangle, AlertCircle, Info, CheckCircle2, X, type LucideIcon } from 'lucide-vue-next'

interface InlineAlertProps {
  tone: 'info' | 'success' | 'warning' | 'danger'
  title: string
  description?: string
  code?: string
  dismissible?: boolean
  /**
   * Override the live-region role.  Danger interrupts by default; other
   * tones announce politely and callers may opt a high-risk warning into an
   * assertive announcement.
   * Allowed values: 'assertive' | 'polite' | 'off'.
   */
  live?: 'assertive' | 'polite' | 'off'
}

const props = withDefaults(defineProps<InlineAlertProps>(), {
  description: '',
  code: '',
  dismissible: false,
  live: undefined,
})

const emit = defineEmits<{
  (e: 'dismiss'): void
}>()

const titleId = useId()
const descId = useId()

const visual = computed<{ icon: LucideIcon; className: string; label: string }>(() => {
  switch (props.tone) {
    case 'success':
      return { icon: CheckCircle2, className: 'inline-alert--success', label: '成功' }
    case 'warning':
      return { icon: AlertTriangle, className: 'inline-alert--warning', label: '需要处理' }
    case 'danger':
      return { icon: AlertCircle, className: 'inline-alert--danger', label: '失败' }
    default:
      return { icon: Info, className: 'inline-alert--info', label: '提示' }
  }
})

const defaultLive = computed<'assertive' | 'polite'>(() => {
  // The tone-based default lets the operator hear a danger immediately
  // while success/info wait politely.  Callers can override.
  return props.tone === 'danger' ? 'assertive' : 'polite'
})

const liveRole = computed(() => {
  const explicit = props.live
  if (explicit === 'off') return undefined
  if (explicit === 'assertive') return 'alert'
  if (explicit === 'polite') return 'status'
  if (defaultLive.value === 'assertive') return 'alert'
  return 'status'
})
</script>

<template>
  <div
    class="inline-alert"
    :class="visual.className"
    :role="liveRole"
    :aria-labelledby="titleId"
    :aria-describedby="description ? descId : undefined"
    data-testid="inline-alert"
  >
    <component :is="visual.icon" class="inline-alert__icon" :size="18" aria-hidden="true" />
    <div class="inline-alert__body">
      <p :id="titleId" class="inline-alert__title">{{ title }}</p>
      <p v-if="description" :id="descId" class="inline-alert__desc">{{ description }}</p>
      <p v-if="code" class="inline-alert__code mono tabular break-anywhere">错误码：{{ code }}</p>
    </div>
    <button
      v-if="dismissible"
      type="button"
      class="inline-alert__dismiss"
      aria-label="关闭提示"
      @click="emit('dismiss')"
    >
      <X :size="14" aria-hidden="true" />
    </button>
  </div>
</template>

<style scoped>
.inline-alert {
  display: flex;
  align-items: flex-start;
  gap: var(--pd-space-12);
  padding: var(--pd-space-12) var(--pd-space-16);
  border-radius: var(--pd-radius-sm);
  border: 1px solid var(--pd-border-separator);
  border-inline-start-width: 2px;
  border-inline-start-color: var(--inline-alert-feedback);
  background: var(--inline-alert-background);
  color: var(--inline-alert-feedback);
}

.inline-alert__icon {
  flex-shrink: 0;
  margin-top: 2px;
}

.inline-alert__body {
  display: flex;
  flex-direction: column;
  gap: var(--pd-space-4);
  min-width: 0;
  flex: 1;
}

.inline-alert__title {
  font-size: var(--pd-font-size-13);
  font-weight: var(--pd-font-weight-medium);
  color: var(--pd-text-default);
  overflow-wrap: anywhere;
}

.inline-alert__desc {
  font-size: var(--pd-font-size-13);
  color: var(--pd-text-muted);
  overflow-wrap: anywhere;
}

.inline-alert__code {
  font-size: var(--pd-font-size-11);
  color: var(--pd-text-subtle);
  overflow-wrap: anywhere;
}

.inline-alert__dismiss {
  flex-shrink: 0;
  width: var(--pd-control-height-sm);
  height: var(--pd-control-height-sm);
  border-radius: var(--pd-radius-sm);
  display: inline-flex;
  align-items: center;
  justify-content: center;
  color: var(--pd-text-muted);
  background: transparent;
  border: 1px solid transparent;
}

.inline-alert__dismiss:hover {
  background: var(--pd-control-bg-hovered);
  color: var(--pd-text-default);
}

.inline-alert--info {
  --inline-alert-feedback: var(--pd-feedback-info);
  --inline-alert-background: var(--pd-feedback-info-subtle);
}

.inline-alert--success {
  --inline-alert-feedback: var(--pd-feedback-success);
  --inline-alert-background: var(--pd-feedback-success-subtle);
}

.inline-alert--warning {
  --inline-alert-feedback: var(--pd-feedback-warning);
  --inline-alert-background: var(--pd-feedback-warning-subtle);
}

.inline-alert--danger {
  --inline-alert-feedback: var(--pd-feedback-danger);
  --inline-alert-background: var(--pd-feedback-danger-subtle);
}

@media (max-width: 767px), (pointer: coarse) {
  .inline-alert__dismiss {
    width: var(--pd-control-height-touch);
    height: var(--pd-control-height-touch);
  }
}
</style>
