<script setup lang="ts">
import { computed } from 'vue'

type Variant = 'primary' | 'secondary' | 'danger' | 'ghost'
type Size = 'sm' | 'md'

export interface AppButtonProps {
  variant?: Variant
  size?: Size
  type?: 'button' | 'submit' | 'reset'
  disabled?: boolean
  loading?: boolean
  /** Optional label shown in place of the slot while loading.  Omit to keep the default slot + spinner. */
  loadingLabel?: string
  fullWidth?: boolean
  ariaLabel?: string
}

const props = withDefaults(defineProps<AppButtonProps>(), {
  variant: 'secondary',
  size: 'md',
  type: 'button',
  disabled: false,
  loading: false,
  loadingLabel: '',
  fullWidth: false,
  ariaLabel: '',
})

const variantClass = computed(() => `app-button--${props.variant}`)
const sizeClass = computed(() => `app-button--${props.size}`)
const isDisabled = computed(() => props.disabled || props.loading)
</script>

<template>
  <button
    :type="type"
    :class="['app-button', variantClass, sizeClass, { 'app-button--block': fullWidth }]"
    :disabled="isDisabled"
    :aria-busy="loading || undefined"
    :aria-label="ariaLabel || undefined"
    :aria-disabled="isDisabled || undefined"
    data-testid="app-button"
  >
    <span v-if="loading" class="app-button__spinner" aria-hidden="true" />
    <span class="app-button__content">
      <slot v-if="!loading || !loadingLabel" />
      <template v-else>{{ loadingLabel }}</template>
    </span>
  </button>
</template>

<style scoped>
.app-button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: var(--pd-space-8);
  font-size: var(--pd-font-size-13);
  font-weight: var(--pd-font-weight-medium);
  border-radius: var(--pd-radius-sm);
  border: 1px solid var(--pd-control-border-default);
  background: var(--pd-control-bg);
  color: var(--pd-text-default);
  padding: 0 var(--pd-space-12);
  height: var(--pd-control-height-md);
  min-width: var(--pd-control-height-md);
  cursor: pointer;
  transition:
    background var(--pd-transition-fast),
    border-color var(--pd-transition-fast),
    color var(--pd-transition-fast);
}

.app-button:hover:not(:disabled) {
  background: var(--pd-control-bg-hovered);
  border-color: var(--pd-control-border-active);
}

.app-button:focus-visible {
  outline: 2px solid var(--pd-border-focus);
  outline-offset: 2px;
}

.app-button:disabled {
  cursor: not-allowed;
  background: var(--pd-control-disabled-bg);
  border-color: var(--pd-control-disabled-border);
  color: var(--pd-control-disabled-text);
}

.app-button--primary {
  background: var(--pd-control-primary-bg);
  color: var(--pd-control-primary-text);
  border-color: var(--pd-control-primary-bg);
}

.app-button--primary:hover:not(:disabled) {
  background: var(--pd-control-primary-bg-hovered);
  border-color: var(--pd-control-primary-bg-hovered);
}

.app-button--danger {
  background: var(--pd-feedback-danger-subtle);
  color: var(--pd-feedback-danger);
  border-color: var(--pd-feedback-danger-muted);
}

.app-button--danger:hover:not(:disabled) {
  background: var(--pd-feedback-danger-muted);
  border-color: var(--pd-feedback-danger);
  color: var(--pd-text-default);
}

.app-button--ghost {
  background: transparent;
  border-color: transparent;
  color: var(--pd-text-muted);
}

.app-button--ghost:hover:not(:disabled) {
  background: var(--pd-control-bg-hovered);
  color: var(--pd-text-default);
}

.app-button--sm {
  height: var(--pd-control-height-sm);
  padding: 0 var(--pd-space-8);
  font-size: var(--pd-font-size-12);
  min-width: var(--pd-control-height-sm);
}

.app-button--block {
  width: 100%;
}

.app-button__spinner {
  width: 12px;
  height: 12px;
  border-radius: 50%;
  border: 2px solid currentColor;
  border-top-color: transparent;
  animation: pd-spin 700ms linear infinite;
}

.app-button__content {
  display: inline-flex;
  align-items: center;
  gap: var(--pd-space-8);
}

@media (prefers-reduced-motion: reduce) {
  .app-button__spinner {
    animation: none;
  }
}

@media (max-width: 767px), (pointer: coarse) {
  .app-button {
    min-height: var(--pd-control-height-touch);
  }

  .app-button--sm {
    min-height: var(--pd-control-height-touch);
  }
}
</style>
