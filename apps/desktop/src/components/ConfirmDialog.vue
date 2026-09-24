<script setup lang="ts">
import { nextTick, onBeforeUnmount, ref, useId, watch } from 'vue'

/** 破坏性或外部可见动作的确认对话框。 */
interface ConfirmDialogProps {
  open: boolean
  title: string
  description: string
  confirmLabel: string
  cancelLabel?: string
  busy?: boolean
}

const props = withDefaults(defineProps<Readonly<ConfirmDialogProps>>(), {
  cancelLabel: '取消',
  busy: false,
})
const emit = defineEmits<{ confirm: []; cancel: [] }>()

const titleId = useId()
const descriptionId = useId()
const dialogRef = ref<HTMLDialogElement | null>(null)
const cancelRef = ref<HTMLButtonElement | null>(null)
let previouslyFocused: HTMLElement | null = null
let appRoot: HTMLElement | null = null
let appRootWasInert = false
let transitionGeneration = 0

function setBackgroundInert() {
  appRoot = document.querySelector<HTMLElement>('#app')
  if (!appRoot) return
  appRootWasInert = appRoot.hasAttribute('inert')
  if (!appRootWasInert) appRoot.setAttribute('inert', '')
}

function restoreBackground() {
  if (appRoot && !appRootWasInert) appRoot.removeAttribute('inert')
  appRoot = null
  appRootWasInert = false
}

async function openDialog() {
  const generation = ++transitionGeneration
  previouslyFocused = document.activeElement instanceof HTMLElement ? document.activeElement : null
  setBackgroundInert()
  await nextTick()
  if (!props.open || generation !== transitionGeneration) return
  const dialog = dialogRef.value
  if (!dialog) return
  if (!dialog.open) {
    if (typeof dialog.showModal === 'function') dialog.showModal()
    else dialog.setAttribute('open', '')
  }
  // 破坏性动作默认聚焦取消，避免一次回车直接执行。
  cancelRef.value?.focus()
}

function closeDialog(restoreFocus = true) {
  transitionGeneration++
  const dialog = dialogRef.value
  if (dialog?.open) {
    if (typeof dialog.close === 'function') dialog.close()
    else dialog.removeAttribute('open')
  }
  restoreBackground()
  if (restoreFocus && previouslyFocused?.isConnected) previouslyFocused.focus()
  previouslyFocused = null
}

function cancel() {
  if (!props.busy) emit('cancel')
}

watch(
  () => props.open,
  (open) => {
    if (open) void openDialog()
    else closeDialog()
  },
  { immediate: true },
)

onBeforeUnmount(() => closeDialog())
</script>

<template>
  <Teleport to="body">
    <dialog
      v-if="open"
      ref="dialogRef"
      class="dialog"
      role="alertdialog"
      aria-modal="true"
      :aria-labelledby="titleId"
      :aria-describedby="descriptionId"
      @cancel.prevent="cancel"
    >
      <h2 :id="titleId" class="dialog__title">{{ title }}</h2>
      <p :id="descriptionId" class="dialog__description">{{ description }}</p>
      <div class="dialog__actions">
        <button
          ref="cancelRef"
          class="button button--secondary"
          type="button"
          :disabled="busy"
          @click="cancel"
        >
          {{ cancelLabel }}
        </button>
        <button
          class="button button--danger"
          type="button"
          :disabled="busy"
          @click="emit('confirm')"
        >
          {{ busy ? '正在处理…' : confirmLabel }}
        </button>
      </div>
    </dialog>
  </Teleport>
</template>

<style scoped>
.dialog {
  width: var(--dialog-width);
  max-width: calc(100vw - (var(--space-5) * 2));
  padding: var(--space-4);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-panel);
  background: var(--bg-surface);
  box-shadow: var(--shadow-overlay);
  color: var(--text-primary);
}

.dialog::backdrop {
  background: var(--mask-overlay);
}

.dialog__title {
  font-size: var(--font-size-section);
  line-height: var(--line-section);
}

.dialog__description {
  margin-top: var(--space-2);
  color: var(--text-secondary);
  font-size: var(--font-size-label);
  line-height: var(--line-label);
}

.dialog__actions {
  display: flex;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: var(--control-gap);
  margin-top: var(--space-4);
}
</style>
