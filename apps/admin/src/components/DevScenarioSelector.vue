<script setup lang="ts">
import { computed } from 'vue'

import { SCENARIO_DESCRIPTORS, type ScenarioId } from '@/data/mock/scenarios'
import { useScenarioSelector } from '@/composables/useScenarioSelector'

const { active, setActive } = useScenarioSelector()

const isDev = import.meta.env.DEV && __ADMIN_MOCK_BUILD__

const currentId = computed<ScenarioId>({
  get: () => active.value,
  set: (value: ScenarioId) => setActive(value),
})
</script>

<template>
  <div v-if="isDev" class="dev-scenario" role="region" aria-label="开发场景切换器">
    <label class="dev-scenario__label" for="dev-scenario-select">Dev 场景</label>
    <select
      id="dev-scenario-select"
      v-model="currentId"
      class="dev-scenario__select"
      data-testid="dev-scenario-select"
    >
      <option
        v-for="descriptor in SCENARIO_DESCRIPTORS"
        :key="descriptor.id"
        :value="descriptor.id"
      >
        {{ descriptor.label }}（{{ descriptor.id }}）
      </option>
    </select>
    <p class="dev-scenario__hint">仅在开发构建中显示，生产构建永不渲染。</p>
  </div>
</template>

<style scoped>
.dev-scenario {
  display: flex;
  align-items: center;
  gap: var(--pd-space-8);
  padding: var(--pd-space-4) var(--pd-shell-content-padding);
  background: var(--pd-feedback-warning-subtle);
  border-bottom: 1px solid var(--pd-feedback-warning-muted);
  font-size: var(--pd-font-size-12);
  color: var(--pd-text-muted);
  flex-wrap: wrap;
}

.dev-scenario__label {
  color: var(--pd-feedback-warning);
  font-weight: var(--pd-font-weight-semibold);
}

.dev-scenario__select {
  flex: 1 1 200px;
  min-width: 0;
  max-width: 100%;
  background: var(--pd-control-bg);
  color: var(--pd-text-default);
  border: 1px solid var(--pd-control-border-default);
  border-radius: var(--pd-radius-sm);
  padding: 0 var(--pd-space-8);
  height: var(--pd-control-height-md);
  font-size: var(--pd-font-size-12);
  font-family: inherit;
}

.dev-scenario__hint {
  color: var(--pd-text-subtle);
  margin: 0;
}

@media (max-width: 767px) {
  .dev-scenario__label,
  .dev-scenario__hint {
    width: 100%;
  }
}
</style>
