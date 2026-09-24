<script setup lang="ts">
import { computed } from 'vue'
import { useRoute } from 'vue-router'

import { runtimeEnvironment } from './environment'

const route = useRoute()
const modeLabel = computed(() => (__ADMIN_PRODUCTION_BUILD__ ? 'Production' : 'Mock'))
const isMock = computed(() => __ADMIN_MOCK_BUILD__)
const buildVersion = computed(() => runtimeEnvironment.buildVersion)
const buildIdentityTitle = computed(() =>
  runtimeEnvironment.buildCommit === 'unknown'
    ? `构建版本 ${buildVersion.value}`
    : `构建版本 ${buildVersion.value} · Source Commit ${runtimeEnvironment.buildCommit}`,
)
const currentPage = computed(() =>
  typeof route.meta['title'] === 'string' ? route.meta['title'] : '总览',
)
</script>

<template>
  <header class="topbar" role="banner">
    <a href="#main-content" class="topbar__skip">跳到主内容</a>
    <h1 id="app-route-title" class="topbar__page">{{ currentPage }}</h1>
    <div class="topbar__right">
      <span
        class="topbar__badge"
        :class="{ 'topbar__badge--mock': isMock, 'topbar__badge--prod': !isMock }"
        :data-testid="isMock ? 'env-badge-mock' : 'env-badge-production'"
        :aria-label="`当前数据源模式：${modeLabel}`"
      >
        {{ modeLabel }}
      </span>
      <span class="topbar__version mono" :title="buildIdentityTitle">v{{ buildVersion }}</span>
    </div>
  </header>
</template>

<style scoped>
.topbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 var(--pd-space-16);
  gap: var(--pd-space-12);
  z-index: var(--pd-z-sticky);
}

.topbar__skip {
  position: absolute;
  left: var(--pd-space-8);
  top: -32px;
  background: var(--pd-container-elevated-bg);
  color: var(--pd-text-default);
  padding: var(--pd-space-4) var(--pd-space-8);
  border: 1px solid var(--pd-border-focus);
  border-radius: var(--pd-radius-sm);
  font-size: var(--pd-font-size-13);
  transition: top var(--pd-transition-fast);
  z-index: var(--pd-z-menu);
}

.topbar__skip:focus {
  top: var(--pd-space-8);
  text-decoration: none;
}

.topbar__page {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--pd-text-default);
  font-size: var(--pd-font-size-14);
  font-weight: var(--pd-font-weight-semibold);
  line-height: var(--pd-line-height-ui);
}

.topbar__right {
  display: flex;
  align-items: center;
  gap: var(--pd-space-12);
  flex-shrink: 0;
}

.topbar__badge {
  display: inline-flex;
  align-items: center;
  gap: var(--pd-space-8);
  padding: 0 var(--pd-space-8);
  height: var(--pd-badge-height);
  border-radius: var(--pd-radius-sm);
  font-size: var(--pd-font-size-12);
  font-weight: var(--pd-font-weight-medium);
  border: 1px solid var(--pd-border-separator);
}

.topbar__badge--mock {
  color: var(--pd-feedback-warning);
  background: var(--pd-feedback-warning-subtle);
  border-color: var(--pd-feedback-warning-muted);
}

.topbar__badge--prod {
  color: var(--pd-feedback-success);
  background: var(--pd-feedback-success-subtle);
  border-color: var(--pd-feedback-success-muted);
}

.topbar__version {
  font-size: var(--pd-font-size-12);
  color: var(--pd-text-subtle);
}

@media (max-width: 767px) {
  .topbar {
    padding-inline: var(--pd-space-12);
  }

  .topbar__version {
    display: none;
  }

  .topbar__right {
    gap: var(--pd-space-4);
  }
}
</style>
