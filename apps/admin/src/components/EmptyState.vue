<script setup lang="ts">
import type { Component } from 'vue'

interface EmptyStateProps {
  icon?: Component
  title: string
  description?: string
  ariaLabel?: string
  variant?: 'panel' | 'bare'
}

withDefaults(defineProps<EmptyStateProps>(), {
  icon: undefined,
  description: '',
  ariaLabel: '',
  variant: 'panel',
})
</script>

<template>
  <div
    :class="['empty-state', `empty-state--${variant}`]"
    role="status"
    :aria-label="ariaLabel || title"
    data-testid="empty-state"
  >
    <component v-if="icon" :is="icon" class="empty-state__icon" :size="20" aria-hidden="true" />
    <p class="empty-state__title">{{ title }}</p>
    <p v-if="description" class="empty-state__desc">{{ description }}</p>
    <div v-if="$slots.action" class="empty-state__action">
      <slot name="action" />
    </div>
  </div>
</template>

<style scoped>
.empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: var(--pd-space-8);
  min-block-size: var(--pd-empty-state-min-block-size);
  text-align: center;
  color: var(--pd-text-subtle);
}

.empty-state--panel {
  padding: var(--pd-space-32) var(--pd-space-16);
  background: var(--pd-container-panel-bg);
  border: 1px solid var(--pd-border-separator);
  border-radius: var(--pd-radius-md);
}

.empty-state--bare {
  padding: var(--pd-space-16) var(--pd-space-8);
}

.empty-state__icon {
  color: var(--pd-text-subtle);
}

.empty-state__title {
  color: var(--pd-text-default);
  font-size: var(--pd-font-size-14);
  font-weight: var(--pd-font-weight-semibold);
}

.empty-state__desc {
  font-size: var(--pd-font-size-13);
  line-height: var(--pd-line-height-reading);
  color: var(--pd-text-muted);
  max-width: 360px;
}

.empty-state__action {
  margin-top: var(--pd-space-8);
}
</style>
