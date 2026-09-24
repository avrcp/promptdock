<script setup lang="ts">
import { Search } from 'lucide-vue-next'

import type { DeviceStateFilter } from '@/contracts/device'
import { DEVICE_STATE_FILTERS } from './useDevicesController'

defineProps<{
  resultCount: number
  hasActiveFilter: boolean
}>()

const emit = defineEmits<{ clear: [] }>()
const state = defineModel<DeviceStateFilter | null>('state', { default: null })
const search = defineModel<string>('search', { default: '' })
</script>

<template>
  <div class="device-filters" role="region" aria-label="设备工具栏">
    <div class="device-filters__states" role="group" aria-label="状态筛选">
      <button
        v-for="option in DEVICE_STATE_FILTERS"
        :key="option.label"
        type="button"
        :aria-pressed="state === option.value"
        :class="[
          'device-filters__state',
          { 'device-filters__state--active': state === option.value },
        ]"
        :data-state="option.value ?? 'all'"
        @click="state = option.value"
      >
        {{ option.label }}
      </button>
    </div>
    <div class="device-filters__search">
      <label for="device-search" class="sr-only">搜索设备</label>
      <Search :size="14" aria-hidden="true" />
      <input
        id="device-search"
        v-model="search"
        type="search"
        placeholder="按名称或设备 ID 搜索"
        data-testid="device-search"
        data-keyboard-search
      />
    </div>
    <div class="device-filters__summary" aria-live="polite">
      <span>{{ resultCount }} 台设备</span>
      <button
        v-if="hasActiveFilter"
        type="button"
        class="device-filters__clear"
        @click="emit('clear')"
      >
        清空筛选
      </button>
    </div>
    <div class="device-filters__actions">
      <slot name="actions" />
    </div>
  </div>
</template>

<style scoped>
.device-filters {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--pd-space-12);
  margin-bottom: var(--pd-space-16);
  flex-wrap: wrap;
}
.device-filters__states {
  display: flex;
  gap: var(--pd-space-4);
  flex-wrap: wrap;
}
.device-filters__state {
  min-height: var(--pd-control-height-md);
  padding: 0 var(--pd-space-8);
  border: 1px solid var(--pd-control-border-default);
  border-radius: var(--pd-radius-sm);
  background: transparent;
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-13);
  cursor: pointer;
  transition:
    background var(--pd-transition-fast),
    border-color var(--pd-transition-fast),
    color var(--pd-transition-fast);
}
.device-filters__state:hover {
  background: var(--pd-control-bg-hovered);
  color: var(--pd-text-default);
}
.device-filters__state--active {
  background: var(--pd-control-bg-selected);
  color: var(--pd-text-default);
  border-color: var(--pd-border-emphasis);
}
.device-filters__search {
  position: relative;
  display: flex;
  align-items: center;
  width: 280px;
  max-width: 100%;
}
.device-filters__search svg {
  position: absolute;
  inset-inline-start: var(--pd-space-8);
  color: var(--pd-text-subtle);
  pointer-events: none;
}
.device-filters__search input {
  width: 100%;
  height: var(--pd-control-height-md);
  padding-inline: 28px var(--pd-space-8);
  border: 1px solid var(--pd-control-border-default);
  border-radius: var(--pd-radius-sm);
  background: var(--pd-control-bg);
  color: var(--pd-text-default);
  font-size: max(16px, var(--pd-font-size-13));
}
.device-filters__search input:focus-visible {
  border-color: var(--pd-border-focus);
  outline: 2px solid var(--pd-border-focus);
  outline-offset: 0;
}
.device-filters__summary {
  display: inline-flex;
  align-items: center;
  gap: var(--pd-space-8);
  margin-inline-start: auto;
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-12);
  line-height: var(--pd-line-height-metadata);
}
.device-filters__actions {
  display: inline-flex;
  align-items: center;
  gap: var(--pd-space-8);
}
.device-filters__clear {
  padding: 0;
  color: var(--pd-text-link);
  font: inherit;
  background: transparent;
  border: 0;
  cursor: pointer;
}
@media (prefers-reduced-motion: no-preference) {
  .device-filters__state:active,
  .device-filters__clear:active {
    transform: translateY(1px);
  }
}
@media (max-width: 767px), (pointer: coarse) {
  .device-filters__search,
  .device-filters__summary,
  .device-filters__actions {
    width: 100%;
  }
  .device-filters__summary {
    margin-inline-start: 0;
  }
  .device-filters__state {
    min-height: var(--pd-control-height-touch);
  }
  .device-filters__clear {
    min-height: var(--pd-control-height-touch);
    padding: 0 var(--pd-space-8);
  }
  .device-filters__actions > :deep(button) {
    flex: 1;
  }
}
</style>
