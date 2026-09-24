<script setup lang="ts">
import NavIcon from './NavIcon.vue'
import type { ViewKey } from '../desktop/views'

interface NavigationItem {
  key: ViewKey
  label: string
  shortLabel: string
  icon: ViewKey
}

interface AppNavigationProps {
  active: ViewKey
}

defineProps<Readonly<AppNavigationProps>>()
const emit = defineEmits<{ navigate: [view: ViewKey] }>()

const items: readonly NavigationItem[] = [
  { key: 'overview', label: '总览', shortLabel: '总览', icon: 'overview' },
  { key: 'integration', label: '桌面与 Hook', shortLabel: '集成', icon: 'integration' },
  { key: 'notifications', label: 'Relay 与通知', shortLabel: '通知', icon: 'notifications' },
  { key: 'deliveries', label: '投递记录', shortLabel: '投递', icon: 'deliveries' },
  { key: 'diagnostics', label: '后台与诊断', shortLabel: '诊断', icon: 'diagnostics' },
]
</script>

<template>
  <nav class="navigation-rail" aria-label="主导航">
    <ul class="navigation-rail__items">
      <li v-for="item in items" :key="item.key">
        <button
          class="navigation-item"
          :class="{ 'navigation-item--active': active === item.key }"
          type="button"
          :aria-label="item.shortLabel === item.label ? item.label : `${item.shortLabel}（${item.label}）`"
          :aria-current="active === item.key ? 'true' : undefined"
          @click="emit('navigate', item.key)"
        >
          <NavIcon :name="item.icon" :size="20" />
          <span class="navigation-item__label">{{ item.shortLabel }}</span>
        </button>
      </li>
    </ul>
  </nav>
</template>

<style scoped>
.navigation-rail {
  /* 占据第 2 行（标题栏之下）到第 4 行（状态栏之下），保持完整高度 */
  grid-row: 2 / 4;
  display: flex;
  min-height: 0;
  flex-direction: column;
  align-items: stretch;
  border-inline-end: 1px solid var(--border-default);
  background: var(--bg-rail);
  overflow-x: hidden;
  overflow-y: auto;
}

.navigation-rail__items {
  display: flex;
  flex: 1;
  flex-direction: column;
  gap: var(--space-1);
  margin: 0;
  padding: var(--space-4) 0 0;
  list-style: none;
}

.navigation-rail__items > li:last-child {
  margin-top: auto;
  margin-bottom: var(--space-2);
}

.navigation-item {
  position: relative;
  display: grid;
  min-height: var(--nav-item-height);
  width: 100%;
  padding: var(--nav-item-padding);
  place-items: center;
  gap: var(--space-1);
  border: 0;
  background: transparent;
  color: var(--text-muted);
  cursor: pointer;
  transition:
    background-color var(--duration-hover) var(--ease-out),
    color var(--duration-hover) var(--ease-out);
}

/* 选中指示条：颜色之外的第二条线索由 ::before 的位置与宽度承担 */
.navigation-item::before {
  position: absolute;
  top: var(--space-2);
  bottom: var(--space-2);
  inset-inline-start: 0;
  width: var(--nav-indicator-width);
  border-radius: var(--nav-indicator-radius);
  background: transparent;
  content: '';
}

@media (hover: hover) {
  .navigation-item:hover {
    background: var(--bg-surface);
    color: var(--text-primary);
  }
}

.navigation-item:focus-visible {
  background: var(--bg-surface);
  color: var(--text-primary);
}

.navigation-item--active {
  background: var(--brand-soft);
  color: var(--brand-primary);
}

.navigation-item--active::before {
  background: var(--brand-primary);
}

.navigation-item__label {
  max-width: var(--nav-label-max-width);
  overflow: hidden;
  font-size: var(--font-size-meta);
  font-weight: var(--weight-medium);
  line-height: var(--line-nano);
  text-align: center;
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>
