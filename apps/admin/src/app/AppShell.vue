<script setup lang="ts">
import { nextTick, ref, watch } from 'vue'
import { useRoute } from 'vue-router'

import AppSidebar from './AppSidebar.vue'
import AppTopbar from './AppTopbar.vue'
import DevScenarioSelector from '@/components/DevScenarioSelector.vue'
import SessionStatusBar from '@/components/SessionStatusBar.vue'
import KeyboardShortcuts from './KeyboardShortcuts.vue'

const route = useRoute()
const mainScrollRegion = ref<HTMLElement | null>(null)

watch(
  () => route.path,
  async (path, previousPath) => {
    if (path === previousPath) return
    await nextTick()
    mainScrollRegion.value?.scrollTo({ top: 0, left: 0, behavior: 'auto' })
  },
  { flush: 'post' },
)
</script>

<template>
  <div class="app-shell">
    <AppSidebar class="app-shell__sidebar" />
    <div class="app-shell__workspace" data-testid="app-workspace">
      <AppTopbar class="app-shell__topbar" />
      <SessionStatusBar />
      <DevScenarioSelector />
      <main
        id="main-content"
        ref="mainScrollRegion"
        class="app-shell__content"
        tabindex="-1"
        aria-labelledby="app-route-title"
      >
        <div class="app-shell__workbench">
          <RouterView />
        </div>
      </main>
    </div>
    <KeyboardShortcuts />
  </div>
</template>

<style scoped>
.app-shell {
  display: grid;
  grid-template-columns: var(--pd-shell-sidebar-width) minmax(0, 1fr);
  width: 100%;
  height: 100dvh;
  min-width: 0;
  min-height: 0;
  overflow: hidden;
  background: var(--pd-container-workspace-bg);
  color: var(--pd-text-default);
}

.app-shell__workspace {
  display: grid;
  grid-template-rows: var(--pd-shell-topbar-height) auto auto minmax(0, 1fr);
  height: 100dvh;
  min-height: 0;
  min-width: 0;
  overflow: hidden;
}

.app-shell__topbar {
  border-bottom: 1px solid var(--pd-border-separator);
  background: var(--pd-container-header-bg);
}

.app-shell__sidebar {
  height: 100dvh;
  min-height: 0;
  min-width: 0;
  border-inline-end: 1px solid var(--pd-border-separator);
  background: var(--pd-container-sidebar-bg);
}

.app-shell__content {
  min-width: 0;
  min-height: 0;
  overflow: auto;
  overscroll-behavior: contain;
  scrollbar-gutter: stable;
}

.app-shell__workbench {
  width: min(100%, var(--pd-shell-content-max-width));
  min-width: 0;
  min-height: 100%;
  margin-inline: auto;
  padding: var(--pd-shell-content-padding);
}

.app-shell__content:focus {
  outline: none;
}

@media (max-width: 1199px) {
  .app-shell {
    grid-template-columns: var(--pd-shell-sidebar-rail-width) minmax(0, 1fr);
  }
}

@media (max-width: 767px) {
  .app-shell {
    display: block;
  }

  .app-shell__workspace {
    height: 100dvh;
  }

  .app-shell__sidebar {
    height: calc(var(--pd-shell-mobile-nav-height) + env(safe-area-inset-bottom));
    border-inline-end: 0;
  }

  .app-shell__workbench {
    padding-bottom: calc(var(--pd-shell-mobile-nav-height) + var(--pd-space-12));
  }
}
</style>
