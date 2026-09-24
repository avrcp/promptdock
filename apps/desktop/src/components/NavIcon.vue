<script setup lang="ts">
import { computed } from 'vue'

type IconName =
  | 'overview'
  | 'integration'
  | 'notifications'
  | 'deliveries'
  | 'diagnostics'
  | 'refresh'
  | 'launch'
  | 'hook'
  | 'relay'
  | 'background'
  | 'arrow'
  | 'warning'
  | 'success'
  | 'close'

interface NavIconProps {
  name: IconName
  size?: number
}

const props = withDefaults(defineProps<Readonly<NavIconProps>>(), { size: 18 })

const paths: Record<IconName, string> = {
  overview: 'M4 4h6v6H4V4Zm10 0h6v6h-6V4ZM4 14h6v6H4v-6Zm10 0h6v6h-6v-6Z',
  integration: 'M5 3h14v5H5V3Zm0 13h14v5H5v-5Zm3-6h8v4H8v-4Z',
  notifications:
    'M12 22a2.5 2.5 0 0 0 2.35-1.65h-4.7A2.5 2.5 0 0 0 12 22Zm7-5-2-2v-4.5a5 5 0 0 0-4-4.9V4a1 1 0 1 0-2 0v1.6a5 5 0 0 0-4 4.9V15l-2 2v1h14v-1Z',
  deliveries: 'm3 6 9-4 9 4-9 4-9-4Zm2 3.2 6 2.67V20l-6-3V9.2Zm8 2.67 6-2.67V17l-6 3v-8.13Z',
  diagnostics:
    'M4 5h16v14H4V5Zm2 2v10h12V7H6Zm1.5 2L10 11.5 7.5 14 6 12.5 7.5 11 6 9.5 7.5 8Zm3.5 5h5v2h-5v-2Z',
  refresh: 'M12 4a8 8 0 0 1 7.45 5H17l3.5 3.5L24 9h-2.45A10 10 0 1 0 22 14h-2a8 8 0 1 1-8-10Z',
  launch: 'm5 12 7-8 7 8-4 1v6H9v-6l-4-1Z',
  hook: 'M7 3v5a5 5 0 0 0 4 4.9V21h2v-8.1A5 5 0 0 0 17 8V3h-2v5a3 3 0 0 1-6 0V3H7Z',
  relay:
    'M7 5a3 3 0 1 0 0 6 3 3 0 0 0 0-6Zm10 8a3 3 0 1 0 0 6 3 3 0 0 0 0-6ZM9.7 9.5l4.6 3 .9-1.5-4.6-3-.9 1.5Z',
  background: 'M5 5h14v14H5V5Zm3 3v8h8V8H8Z',
  arrow: 'm9 5 7 7-7 7-1.5-1.5L13 12 7.5 6.5 9 5Z',
  warning: 'M12 3 1 21h22L12 3Zm0 5 7.5 12h-15L12 8Zm-1 4v4h2v-4h-2Zm0 5v2h2v-2h-2Z',
  success:
    'M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20Zm-1.4 14.2-4-4L8 10.8l2.6 2.6L16 8l1.4 1.4-6.8 6.8Z',
  close:
    'M6.7 5.3 12 10.6l5.3-5.3 1.4 1.4-5.3 5.3 5.3 5.3-1.4 1.4-5.3-5.3-5.3 5.3-1.4-1.4 5.3-5.3-5.3-5.3 1.4-1.4Z',
}

const path = computed(() => paths[props.name])
</script>

<template>
  <svg
    class="nav-icon"
    :width="size"
    :height="size"
    viewBox="0 0 24 24"
    fill="currentColor"
    focusable="false"
    aria-hidden="true"
  >
    <path :d="path" />
  </svg>
</template>

<style scoped>
.nav-icon {
  display: block;
  flex: none;
}
</style>
