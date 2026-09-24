<script setup lang="ts">
import { navigationItems } from './navigation'
</script>

<template>
  <nav class="sidebar" aria-label="主导航">
    <ul class="sidebar__list">
      <li v-for="item in navigationItems" :key="item.path">
        <RouterLink
          :to="item.path"
          class="sidebar__link"
          active-class="sidebar__link--active"
          :title="item.description"
          :aria-label="item.label"
        >
          <component :is="item.icon" class="sidebar__icon" aria-hidden="true" :size="16" />
          <span class="sidebar__label">{{ item.label }}</span>
        </RouterLink>
      </li>
    </ul>
  </nav>
</template>

<style scoped>
.sidebar {
  display: flex;
  flex-direction: column;
  min-width: 0;
  height: 100%;
  padding: calc(var(--pd-shell-topbar-height) + var(--pd-space-8)) var(--pd-space-8)
    var(--pd-space-8);
  overflow-y: auto;
}

.sidebar__list {
  display: grid;
  gap: var(--pd-space-2);
}

.sidebar__link {
  position: relative;
  display: flex;
  align-items: center;
  gap: var(--pd-space-12);
  height: var(--pd-control-height-md);
  padding: 0 var(--pd-space-12);
  border-radius: var(--pd-radius-sm);
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-13);
  text-decoration: none;
  min-width: 0;
  transition:
    background var(--pd-transition-fast),
    color var(--pd-transition-fast);
}

.sidebar__link:hover {
  background: var(--pd-control-bg-hovered);
  color: var(--pd-text-default);
  text-decoration: none;
}

.sidebar__link--active {
  background: var(--pd-control-bg-selected);
  color: var(--pd-text-default);
  font-weight: var(--pd-font-weight-medium);
}

.sidebar__link--active::before {
  position: absolute;
  inset-block: var(--pd-space-4);
  left: 0;
  width: 2px;
  border-radius: var(--pd-radius-xs);
  background: var(--pd-primary);
  content: '';
}

.sidebar__icon {
  flex: 0 0 auto;
  color: var(--pd-text-subtle);
}

.sidebar__link--active .sidebar__icon {
  color: var(--pd-primary);
}

.sidebar__label {
  flex: 1 1 auto;
  min-width: 0;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

@media (max-width: 1199px) and (min-width: 768px) {
  .sidebar {
    padding-inline: var(--pd-space-8);
  }

  .sidebar__label {
    display: none;
  }

  .sidebar__link {
    justify-content: center;
    padding-inline: 0;
    height: var(--pd-control-height-touch);
  }
}

@media (max-width: 767px) {
  .sidebar {
    position: fixed;
    right: 0;
    bottom: 0;
    left: 0;
    z-index: var(--pd-z-sticky);
    height: calc(var(--pd-shell-mobile-nav-height) + env(safe-area-inset-bottom));
    padding: 0 var(--pd-space-4) env(safe-area-inset-bottom);
    overflow: hidden;
    border-top: 1px solid var(--pd-border-separator);
    background: var(--pd-container-sidebar-bg);
  }

  .sidebar__list {
    grid-template-columns: repeat(5, minmax(0, 1fr));
    gap: 0;
    width: 100%;
  }

  .sidebar__link {
    justify-content: center;
    flex-direction: column;
    gap: var(--pd-space-2);
    height: var(--pd-shell-mobile-nav-height);
    padding: 0 var(--pd-space-2);
    border-radius: 0;
    font-size: var(--pd-font-size-11);
  }

  .sidebar__link--active::before {
    inset: 0 var(--pd-space-12) auto;
    width: auto;
    height: 2px;
  }

  .sidebar__label {
    display: block;
    max-width: 100%;
  }
}
</style>
