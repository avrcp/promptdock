<script setup lang="ts">
import { computed, nextTick, ref, useId } from 'vue'

interface AppTab {
  id: string
  label: string
  count?: number | null
}

interface AppTabsProps {
  tabs: readonly AppTab[]
  modelValue: string
  ariaLabel?: string
  /**
   * Optional base id used to derive deterministic tab/panel ids so a
   * panel can be linked via `aria-controls`.  When omitted, a random id
   * is generated per mount; consumers that need a stable id should pass
   * a meaningful base.
   */
  baseId?: string
}

const props = withDefaults(defineProps<AppTabsProps>(), {
  ariaLabel: '页面标签',
  baseId: '',
})

const emit = defineEmits<{
  (e: 'update:modelValue', value: string): void
}>()

const fallbackId = useId()
const resolvedBase = computed(() => props.baseId || fallbackId)
const tabRefs = ref<HTMLButtonElement[]>([])

function tabId(tab: string): string {
  return `${resolvedBase.value}-tab-${tab}`
}
function panelId(tab: string): string {
  return `${resolvedBase.value}-panel-${tab}`
}

async function activateTab(id: string): Promise<void> {
  emit('update:modelValue', id)
  await nextTick()
  tabRefs.value.find((tab) => tab.dataset.tabId === id)?.focus()
}

function onTabKeydown(event: KeyboardEvent): void {
  const current = props.modelValue
  const ids = props.tabs.map((t) => t.id)
  const index = ids.indexOf(current)
  let next: string | null = null
  if (event.key === 'ArrowRight') {
    next = ids[(index + 1) % ids.length] ?? null
  } else if (event.key === 'ArrowLeft') {
    next = ids[(index - 1 + ids.length) % ids.length] ?? null
  } else if (event.key === 'Home') {
    next = ids[0] ?? null
  } else if (event.key === 'End') {
    next = ids[ids.length - 1] ?? null
  }
  if (next !== null) {
    event.preventDefault()
    void activateTab(next)
  }
}
</script>

<template>
  <div class="app-tabs" role="tablist" :aria-label="ariaLabel" data-testid="app-tabs">
    <button
      v-for="tab in tabs"
      :key="tab.id"
      ref="tabRefs"
      type="button"
      :id="tabId(tab.id)"
      role="tab"
      :aria-selected="tab.id === modelValue"
      :aria-controls="panelId(tab.id)"
      :tabindex="tab.id === modelValue ? 0 : -1"
      :class="['app-tabs__tab', { 'app-tabs__tab--active': tab.id === modelValue }]"
      :data-tab-id="tab.id"
      @click="emit('update:modelValue', tab.id)"
      @keydown="onTabKeydown"
    >
      <span>{{ tab.label }}</span>
      <span v-if="tab.count !== undefined && tab.count !== null" class="app-tabs__count tabular">
        {{ tab.count }}
      </span>
    </button>
  </div>
</template>

<style scoped>
.app-tabs {
  display: flex;
  gap: var(--pd-space-16);
  min-width: 0;
  overflow-x: auto;
  border-bottom: 1px solid var(--pd-border-separator);
}

.app-tabs__tab {
  display: inline-flex;
  align-items: center;
  flex: 0 0 auto;
  gap: var(--pd-space-8);
  height: var(--pd-control-height-md);
  padding: 0 var(--pd-space-8);
  font-size: var(--pd-font-size-13);
  font-weight: var(--pd-font-weight-medium);
  line-height: var(--pd-line-height-ui);
  color: var(--pd-text-muted);
  background: transparent;
  border: 1px solid transparent;
  border-bottom: 2px solid transparent;
  border-radius: var(--pd-radius-sm) var(--pd-radius-sm) 0 0;
  cursor: pointer;
  transition:
    color var(--pd-transition-fast),
    background var(--pd-transition-fast),
    border-color var(--pd-transition-fast);
}

.app-tabs__tab:hover {
  color: var(--pd-text-default);
  background: var(--pd-control-bg-hovered);
}

.app-tabs__tab--active {
  color: var(--pd-text-default);
  border-bottom-color: var(--pd-primary);
}

.app-tabs__count {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: var(--pd-space-16);
  height: var(--pd-space-16);
  padding: 0 var(--pd-space-4);
  border-radius: 999px;
  background: var(--pd-container-header-bg);
  color: var(--pd-text-subtle);
  font-size: var(--pd-font-size-11);
  line-height: var(--pd-line-height-metadata);
}

.app-tabs__tab--active .app-tabs__count {
  color: var(--pd-text-default);
  background: var(--pd-control-bg-selected);
}

@media (max-width: 767px), (pointer: coarse) {
  .app-tabs__tab {
    min-height: var(--pd-control-height-touch);
  }
}
</style>
