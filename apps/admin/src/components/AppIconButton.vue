<script setup lang="ts">
import { computed } from 'vue'
import type { Component } from 'vue'

interface AppIconButtonProps {
  icon: Component
  ariaLabel: string
  size?: 'sm' | 'md'
  tone?: 'default' | 'danger' | 'primary'
  disabled?: boolean
}

const props = withDefaults(defineProps<AppIconButtonProps>(), {
  size: 'md',
  tone: 'default',
  disabled: false,
})

const sizeClass = computed(() => `app-icon-button--${props.size}`)
const toneClass = computed(() => `app-icon-button--${props.tone}`)
</script>

<template>
  <button
    type="button"
    :class="['app-icon-button', sizeClass, toneClass]"
    :disabled="disabled"
    :aria-label="ariaLabel"
    :title="ariaLabel"
    data-testid="app-icon-button"
  >
    <component :is="icon" :size="size === 'sm' ? 14 : 16" aria-hidden="true" />
  </button>
</template>

<style scoped>
.app-icon-button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  flex: 0 0 auto;
  border: 1px solid var(--pd-control-border-default);
  background: var(--pd-control-bg);
  color: var(--pd-text-muted);
  border-radius: var(--pd-radius-sm);
  cursor: pointer;
  transition:
    background var(--pd-transition-fast),
    border-color var(--pd-transition-fast),
    color var(--pd-transition-fast);
}

.app-icon-button--md {
  width: var(--pd-icon-button-size-md);
  height: var(--pd-icon-button-size-md);
}

.app-icon-button--sm {
  width: var(--pd-icon-button-size-sm);
  height: var(--pd-icon-button-size-sm);
}

.app-icon-button:hover:not(:disabled) {
  background: var(--pd-control-bg-hovered);
  border-color: var(--pd-control-border-active);
  color: var(--pd-text-default);
}

.app-icon-button:focus-visible {
  outline: 2px solid var(--pd-border-focus);
  outline-offset: 2px;
}

.app-icon-button:disabled {
  cursor: not-allowed;
  background: var(--pd-control-disabled-bg);
  border-color: var(--pd-control-disabled-border);
  color: var(--pd-control-disabled-text);
}

.app-icon-button--danger {
  color: var(--pd-feedback-danger);
}

.app-icon-button--danger:hover:not(:disabled) {
  background: var(--pd-feedback-danger-subtle);
  border-color: var(--pd-feedback-danger-muted);
  color: var(--pd-feedback-danger);
}

.app-icon-button--primary {
  color: var(--pd-primary);
}

.app-icon-button--primary:hover:not(:disabled) {
  background: var(--pd-primary-subtle);
  border-color: var(--pd-primary-muted);
  color: var(--pd-primary);
}

@media (max-width: 767px), (pointer: coarse) {
  .app-icon-button,
  .app-icon-button--sm,
  .app-icon-button--md {
    width: var(--pd-control-height-touch);
    height: var(--pd-control-height-touch);
  }
}
</style>
