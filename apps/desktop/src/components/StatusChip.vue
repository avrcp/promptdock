<script setup lang="ts">
/**
 * 状态徽标。颜色之外强制提供字形线索，状态绝不由颜色单独承载。
 */
export type StatusTone = 'neutral' | 'success' | 'warning' | 'danger'

interface StatusChipProps {
  tone?: StatusTone
  label: string
}

const props = withDefaults(defineProps<Readonly<StatusChipProps>>(), { tone: 'neutral' })

const glyphs: Record<StatusTone, string> = {
  neutral: '·',
  success: '✓',
  warning: '!',
  danger: '✕',
}
</script>

<template>
  <span class="chip" :class="`chip--${props.tone}`">
    <span class="chip__glyph" aria-hidden="true">{{ glyphs[props.tone] }}</span>
    {{ props.label }}
  </span>
</template>
