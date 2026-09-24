<script setup lang="ts">
import { computed } from 'vue'

type SkeletonVariant = 'text' | 'control' | 'table-row' | 'metric' | 'panel'

interface SkeletonBlockProps {
  variant?: SkeletonVariant
  width?: string
  announce?: boolean
}

const props = withDefaults(defineProps<SkeletonBlockProps>(), {
  variant: 'text',
  width: '100%',
  announce: false,
})

const variantHeight: Record<SkeletonVariant, string> = {
  text: '16px',
  control: '32px',
  'table-row': '44px',
  metric: '64px',
  panel: '96px',
}

const skeletonStyle = computed(() => ({
  width: props.width,
  height: variantHeight[props.variant],
}))
</script>

<template>
  <span
    class="skeleton"
    :style="skeletonStyle"
    :role="announce ? 'status' : undefined"
    :aria-busy="announce ? 'true' : undefined"
    aria-hidden="true"
    data-testid="skeleton"
  />
</template>

<style scoped>
.skeleton {
  display: inline-block;
  border-radius: var(--pd-radius-sm);
  background: var(--pd-control-bg-hovered);
  animation: pd-skeleton-pulse 1200ms ease-in-out infinite alternate;
}

@media (prefers-reduced-motion: reduce) {
  .skeleton {
    animation: none;
    background: var(--pd-control-bg-hovered);
  }
}
</style>
