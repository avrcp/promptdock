<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import NavIcon from './NavIcon.vue'

const SUCCESS_TIMEOUT_MS = 4_500

interface StatusToastProps {
  tone?: 'success' | 'danger'
  message?: string
}

const props = withDefaults(defineProps<Readonly<StatusToastProps>>(), {
  tone: 'success',
  message: '',
})
const emit = defineEmits<{ dismiss: [] }>()

const remaining = ref(SUCCESS_TIMEOUT_MS)
let startedAt = 0
let timer: ReturnType<typeof setTimeout> | null = null

const isPersistent = computed(() => props.tone === 'danger')

function stopTimer() {
  if (timer !== null) {
    clearTimeout(timer)
    timer = null
  }
}

function startTimer() {
  stopTimer()
  if (!props.message || isPersistent.value || remaining.value <= 0) return
  startedAt = Date.now()
  timer = setTimeout(() => {
    timer = null
    emit('dismiss')
  }, remaining.value)
}

function pauseTimer() {
  if (timer === null) return
  remaining.value = Math.max(0, remaining.value - (Date.now() - startedAt))
  stopTimer()
}

function dismiss() {
  stopTimer()
  emit('dismiss')
}

watch(
  () => [props.message, props.tone] as const,
  () => {
    remaining.value = SUCCESS_TIMEOUT_MS
    startTimer()
  },
  { immediate: true },
)

onBeforeUnmount(stopTimer)
</script>

<template>
  <div
    class="toast-region"
    :role="tone === 'danger' ? 'alert' : 'status'"
    aria-atomic="true"
  >
    <Transition name="toast">
      <div
        v-if="message"
        class="status-toast"
        :class="`status-toast--${tone}`"
        @mouseenter="pauseTimer"
        @mouseleave="startTimer"
        @focusin="pauseTimer"
        @focusout="startTimer"
      >
        <NavIcon class="status-toast__icon" :name="tone === 'danger' ? 'warning' : 'success'" />
        <span class="status-toast__message">{{ message }}</span>
        <button class="status-toast__close" type="button" aria-label="关闭提示" @click="dismiss">
          <NavIcon name="close" :size="14" />
        </button>
      </div>
    </Transition>
  </div>
</template>
