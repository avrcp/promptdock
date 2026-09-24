<script setup lang="ts">
import { useRoute, useRouter } from 'vue-router'
import { ArrowLeft, FileQuestion } from 'lucide-vue-next'

import AppButton from '@/components/AppButton.vue'

const router = useRouter()
const route = useRoute()

function goHome(): void {
  void router.push('/overview')
}
</script>

<template>
  <section class="not-found" aria-labelledby="not-found-title">
    <FileQuestion class="not-found__icon" :size="24" aria-hidden="true" />
    <h2 id="not-found-title" class="not-found__title">页面不存在</h2>
    <p class="not-found__desc">请求的路径不在管理控制台范围内。</p>
    <p class="not-found__path mono break-anywhere" data-testid="not-found-path">
      {{ route.fullPath }}
    </p>
    <AppButton variant="primary" data-testid="not-found-home" @click="goHome">
      <ArrowLeft :size="16" aria-hidden="true" />
      <span>返回总览</span>
    </AppButton>
  </section>
</template>

<style scoped>
.not-found {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: var(--pd-space-8);
  min-height: min(520px, 68dvh);
  padding: var(--pd-space-32) var(--pd-space-16);
  text-align: center;
}

.not-found__icon {
  color: var(--pd-text-subtle);
}

.not-found__title {
  color: var(--pd-text-default);
  font-size: var(--pd-font-size-18);
  font-weight: var(--pd-font-weight-semibold);
  line-height: var(--pd-line-height-compact);
}

.not-found__desc {
  max-width: 32rem;
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-13);
  line-height: var(--pd-line-height-reading);
}

.not-found__path {
  max-width: min(100%, 36rem);
  margin-bottom: var(--pd-space-8);
  overflow-wrap: anywhere;
  color: var(--pd-text-subtle);
  font-family: var(--pd-font-code);
  font-size: var(--pd-font-size-12);
  line-height: var(--pd-line-height-code);
}

@media (max-height: 520px) {
  .not-found {
    min-height: auto;
    padding-block: var(--pd-space-24);
  }
}
</style>
