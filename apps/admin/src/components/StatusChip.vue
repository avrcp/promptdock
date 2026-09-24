<script setup lang="ts">
import { computed } from 'vue'

import type { HealthTone } from '@/contracts/common'

interface StatusChipProps {
  tone: HealthTone
  label: string
  ariaLabel?: string
  /**
   * Whether the chip should be announced to assistive tech.  Off by
   * default because StatusChip is ubiquitous in this app and treating
   * every chip as a live region floods screen readers with polite
   * announcements on every refresh.
   */
  live?: 'off' | 'polite' | 'assertive'
}

interface ToneVisual {
  className: string
}

const TONE_VISUALS: Record<HealthTone, ToneVisual> = {
  success: {
    className: 'status-chip--success',
  },
  info: {
    className: 'status-chip--info',
  },
  warning: {
    className: 'status-chip--warning',
  },
  danger: {
    className: 'status-chip--danger',
  },
  muted: {
    className: 'status-chip--muted',
  },
}

const props = withDefaults(defineProps<StatusChipProps>(), {
  ariaLabel: '',
  live: 'off',
})

const visual = computed(() => TONE_VISUALS[props.tone])
const finalAriaLabel = computed(() => props.ariaLabel || `状态：${props.label}`)
const liveRole = computed(() => {
  if (props.live === 'off') return undefined
  return props.live === 'assertive' ? 'alert' : 'status'
})
</script>

<template>
  <span
    class="status-chip"
    :class="visual.className"
    :role="liveRole"
    :aria-label="finalAriaLabel"
    :title="label"
    data-testid="status-chip"
  >
    <span class="status-chip__dot" aria-hidden="true" />
    <span class="status-chip__label">{{ label }}</span>
  </span>
</template>

<style scoped>
.status-chip {
  display: inline-flex;
  align-items: center;
  gap: var(--pd-space-4);
  padding: 0 var(--pd-space-8);
  height: var(--pd-status-chip-height);
  min-width: 0;
  max-width: min(100%, var(--pd-status-chip-max-inline-size));
  border-radius: var(--pd-radius-sm);
  font-size: var(--pd-font-size-12);
  font-weight: var(--pd-font-weight-medium);
  border: 1px solid var(--pd-border-separator);
  background: var(--pd-container-content-bg);
  color: var(--pd-text-default);
  white-space: nowrap;
}

.status-chip__dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--pd-text-subtle);
  flex: 0 0 auto;
}

.status-chip__label {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.status-chip--success {
  background: var(--pd-feedback-success-subtle);
  border-color: var(--pd-feedback-success-muted);
}

.status-chip--success .status-chip__dot {
  background: var(--pd-feedback-success);
}

.status-chip--info {
  background: var(--pd-feedback-info-subtle);
  border-color: var(--pd-feedback-info-muted);
}

.status-chip--info .status-chip__dot {
  background: var(--pd-feedback-info);
}

.status-chip--warning {
  background: var(--pd-feedback-warning-subtle);
  border-color: var(--pd-feedback-warning-muted);
}

.status-chip--warning .status-chip__dot {
  background: var(--pd-feedback-warning);
}

.status-chip--danger {
  background: var(--pd-feedback-danger-subtle);
  border-color: var(--pd-feedback-danger-muted);
}

.status-chip--danger .status-chip__dot {
  background: var(--pd-feedback-danger);
}

.status-chip--muted {
  color: var(--pd-text-muted);
  background: var(--pd-container-content-bg);
  border-color: var(--pd-border-separator);
}
</style>
