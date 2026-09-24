<script setup lang="ts">
import { computed } from 'vue'
import type { Component } from 'vue'

interface StatCardProps {
  label: string
  value: string
  hint?: string
  tone?: 'success' | 'info' | 'warning' | 'danger' | 'muted'
  icon?: Component
  loading?: boolean
}

const props = withDefaults(defineProps<StatCardProps>(), {
  hint: '',
  tone: 'info',
  icon: undefined,
  loading: false,
})

const toneClass = computed(() => `stat-card--${props.tone}`)
const ariaLabel = computed(() => `${props.label}：${props.value}`)
</script>

<template>
  <div
    class="stat-card"
    :class="toneClass"
    :aria-label="ariaLabel"
    :aria-busy="loading || undefined"
    data-testid="stat-card"
  >
    <div class="stat-card__top">
      <span class="stat-card__label">{{ label }}</span>
      <component v-if="icon" :is="icon" class="stat-card__icon" :size="16" aria-hidden="true" />
    </div>
    <p v-if="loading" class="stat-card__value stat-card__value--loading" aria-hidden="true">
      &mdash;&mdash;
    </p>
    <p v-else class="stat-card__value tabular">{{ value }}</p>
    <p v-if="hint" class="stat-card__hint">{{ hint }}</p>
  </div>
</template>

<style scoped>
.stat-card {
  display: flex;
  flex-direction: column;
  gap: var(--pd-space-8);
  min-width: 0;
  padding: var(--pd-space-16);
  background: var(--pd-container-panel-bg);
  position: relative;
}

.stat-card::before {
  position: absolute;
  inset-block: 0;
  inset-inline-start: 0;
  width: 2px;
  background: var(--stat-card-rail);
  content: '';
}

.stat-card--success {
  --stat-card-rail: var(--pd-feedback-success);
}

.stat-card--info {
  --stat-card-rail: var(--pd-feedback-info);
}

.stat-card--warning {
  --stat-card-rail: var(--pd-feedback-warning);
}

.stat-card--danger {
  --stat-card-rail: var(--pd-feedback-danger);
}

.stat-card--muted {
  --stat-card-rail: var(--pd-text-subtle);
}

.stat-card__top {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--pd-space-8);
}

.stat-card__label {
  font-size: var(--pd-font-size-12);
  color: var(--pd-text-muted);
  font-weight: var(--pd-font-weight-medium);
}

.stat-card__icon {
  color: var(--pd-text-subtle);
  flex-shrink: 0;
}

.stat-card--success .stat-card__icon {
  color: var(--pd-feedback-success);
}

.stat-card--info .stat-card__icon {
  color: var(--pd-feedback-info);
}

.stat-card--warning .stat-card__icon {
  color: var(--pd-feedback-warning);
}

.stat-card--danger .stat-card__icon {
  color: var(--pd-feedback-danger);
}

.stat-card__value {
  font-size: var(--pd-font-size-18);
  font-weight: var(--pd-font-weight-semibold);
  color: var(--pd-text-default);
  line-height: var(--pd-line-height-compact);
}

.stat-card__value--loading {
  color: var(--pd-text-subtle);
}

.stat-card__hint {
  font-size: var(--pd-font-size-12);
  color: var(--pd-text-subtle);
}
</style>
