<script setup lang="ts">
import { computed } from 'vue'
import { AlertTriangle, CheckCircle2, Loader2, ShieldOff } from 'lucide-vue-next'

import { useAdminSession } from '@/composables/useAdminSession'
import AppButton from '@/components/AppButton.vue'

const session = useAdminSession()

const visual = computed<{
  tone: 'success' | 'warning' | 'danger' | 'muted'
  label: string
  description: string | null
  retryable: boolean
  icon: typeof AlertTriangle
}>(() => {
  const kind = session.state.value.kind
  if (kind === 'unsupported') {
    return {
      tone: 'muted',
      label: 'Mock 模式',
      description: null,
      retryable: false,
      icon: CheckCircle2,
    }
  }
  if (kind === 'loading') {
    return {
      tone: 'warning',
      label: '正在读取能力',
      description: '所有写操作暂时不可用。',
      retryable: false,
      icon: Loader2,
    }
  }
  if (kind === 'unavailable') {
    return {
      tone: 'danger',
      label: '能力读取失败',
      description: session.state.value.error.message,
      retryable: true,
      icon: AlertTriangle,
    }
  }
  // kind === 'ready'
  if (session.isReadOnly.value) {
    return {
      tone: 'warning',
      label: '只读模式',
      description: session.readOnlyReason.value,
      retryable: false,
      icon: ShieldOff,
    }
  }
  if (session.mode.value === 'partial') {
    return {
      tone: 'warning',
      label: '部分 Operator 权限',
      description: '部分写操作不可用；每个操作附近会说明缺少的能力。',
      retryable: false,
      icon: ShieldOff,
    }
  }
  return {
    tone: 'success',
    label: 'Operator',
    description: null,
    retryable: false,
    icon: CheckCircle2,
  }
})

async function retry(): Promise<void> {
  await session.refresh()
}
</script>

<template>
  <div
    v-if="visual.tone !== 'success' && visual.tone !== 'muted'"
    class="session-bar"
    :class="`session-bar--${visual.tone}`"
    role="status"
    aria-live="polite"
    data-testid="session-status-bar"
  >
    <component :is="visual.icon" :size="16" aria-hidden="true" class="session-bar__icon" />
    <div class="session-bar__text">
      <span class="session-bar__label">{{ visual.label }}</span>
      <span v-if="visual.description" class="session-bar__desc" :title="visual.description">
        {{ visual.description }}
      </span>
    </div>
    <AppButton
      v-if="visual.retryable"
      size="sm"
      variant="secondary"
      data-testid="session-retry"
      @click="retry"
    >
      重试
    </AppButton>
  </div>
</template>

<style scoped>
.session-bar {
  display: flex;
  align-items: center;
  gap: var(--pd-space-12);
  min-height: var(--pd-control-height-md);
  padding: var(--pd-space-4) var(--pd-shell-content-padding);
  border-bottom: 1px solid var(--pd-border-separator);
  border-inline-start: 2px solid currentColor;
  font-size: var(--pd-font-size-13);
}

.session-bar--warning {
  background: var(--pd-feedback-warning-subtle);
  color: var(--pd-feedback-warning);
}

.session-bar--danger {
  background: var(--pd-feedback-danger-subtle);
  color: var(--pd-feedback-danger);
}

.session-bar__icon {
  flex-shrink: 0;
}

.session-bar__text {
  display: flex;
  align-items: baseline;
  gap: var(--pd-space-12);
  flex: 1;
  min-width: 0;
}

.session-bar__label {
  font-weight: var(--pd-font-weight-semibold);
  white-space: nowrap;
}

.session-bar__desc {
  color: var(--pd-text-default);
  font-size: var(--pd-font-size-13);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  min-width: 0;
}

@media (max-width: 767px) {
  .session-bar {
    align-items: flex-start;
    flex-wrap: wrap;
  }

  .session-bar__text {
    align-items: flex-start;
    flex-direction: column;
    gap: var(--pd-space-2);
  }

  .session-bar__desc {
    overflow: visible;
    white-space: normal;
  }
}
</style>
