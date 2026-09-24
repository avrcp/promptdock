<script setup lang="ts">
import NavIcon from './NavIcon.vue'

/**
 * 空态：说明这里是什么、怎么才会有内容，并给出一个明确的下一步动作。
 */
interface EmptyStateProps {
  icon: 'deliveries' | 'diagnostics'
  title: string
  description: string
  actionLabel?: string
  large?: boolean
}

withDefaults(defineProps<Readonly<EmptyStateProps>>(), {
  actionLabel: undefined,
  large: false,
})

const emit = defineEmits<{ action: [] }>()
</script>

<template>
  <div class="empty-state" :class="{ 'empty-state--large': large }">
    <NavIcon :name="icon" :size="large ? 28 : 24" />
    <strong class="empty-state__title">{{ title }}</strong>
    <span class="empty-state__description">{{ description }}</span>
    <button
      v-if="actionLabel"
      class="button button--secondary"
      type="button"
      @click="emit('action')"
    >
      {{ actionLabel }}
    </button>
  </div>
</template>
