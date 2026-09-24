<script setup lang="ts">
import { useId } from 'vue'

/**
 * 面板骨架：分区标题（h2，自动生成 id 供 aria-labelledby 使用）、
 * 说明、右上角操作位、正文与底部脚注。
 */
interface PanelSectionProps {
  title: string
  description?: string
}

defineProps<Readonly<PanelSectionProps>>()

const headingId = useId()
</script>

<template>
  <section class="panel" :aria-labelledby="headingId">
    <header class="panel__header">
      <div class="panel__heading">
        <h2 :id="headingId" class="panel__title">{{ title }}</h2>
        <p v-if="description" class="panel__description">{{ description }}</p>
      </div>
      <slot name="actions" />
    </header>
    <slot />
    <p v-if="$slots.note" class="panel__note">
      <slot name="note" />
    </p>
  </section>
</template>
