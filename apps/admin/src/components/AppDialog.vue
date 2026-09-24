<script setup lang="ts">
import { computed, nextTick, ref, useId, watch } from 'vue'
import { X } from 'lucide-vue-next'

import AppButton from './AppButton.vue'
import { useOverlayScrollLock } from '@/composables/useOverlayScrollLock'
import { FOCUSABLE_SELECTOR, focusPreferredOrFirst } from '@/utils/focus-management'

interface AppDialogProps {
  open: boolean
  title: string
  description?: string
  primaryLabel?: string
  secondaryLabel?: string
  showSecondary?: boolean
  primaryVariant?: 'primary' | 'danger' | 'secondary'
  primaryLoading?: boolean
  primaryDisabled?: boolean
  /** Locks every dismiss path while a parent-owned workflow is pending. */
  pending?: boolean
  /** Allows a workflow to keep its result visible while refusing dismissal. */
  closable?: boolean
  closeOnBackdrop?: boolean
  closeOnEscape?: boolean
  closeOnSecondary?: boolean
  initialFocus?: 'primary' | 'secondary' | 'close'
  ariaLabelClose?: string
}

const props = withDefaults(defineProps<AppDialogProps>(), {
  description: '',
  primaryLabel: '确认',
  secondaryLabel: '取消',
  showSecondary: true,
  primaryVariant: 'primary',
  primaryLoading: false,
  primaryDisabled: false,
  pending: false,
  closable: true,
  closeOnBackdrop: true,
  closeOnEscape: true,
  closeOnSecondary: true,
  initialFocus: 'secondary',
  ariaLabelClose: '关闭对话框',
})

const emit = defineEmits<{
  (e: 'update:open', value: boolean): void
  (e: 'close'): void
  (e: 'primary'): void
  (e: 'secondary'): void
}>()

const dialogRef = ref<HTMLDivElement | null>(null)
const closeBtnRef = ref<HTMLButtonElement | null>(null)
const primaryRef = ref<HTMLButtonElement | null>(null)
const secondaryRef = ref<HTMLButtonElement | null>(null)
const returnFocus = ref<HTMLElement | null>(null)
const titleId = useId()
const descriptionId = useId()
const { lock, unlock } = useOverlayScrollLock()

const isOpen = computed(() => props.open)
const isPending = computed(() => props.pending || props.primaryLoading)
const canClose = computed(() => props.closable && !isPending.value)

function focusables(): HTMLElement[] {
  if (!dialogRef.value) return []
  return Array.from(dialogRef.value.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR))
}

function resolveEl(ref: HTMLElement | { $el?: HTMLElement } | null): HTMLElement | null {
  if (!ref) return null
  if (ref instanceof HTMLElement) return ref
  const el = (ref as { $el?: HTMLElement }).$el
  return el instanceof HTMLElement ? el : null
}

function focusInitial(): void {
  const preferred =
    props.initialFocus === 'primary'
      ? resolveEl(primaryRef.value)
      : props.initialFocus === 'close'
        ? closeBtnRef.value
        : (resolveEl(secondaryRef.value) ?? resolveEl(primaryRef.value))
  focusPreferredOrFirst(dialogRef.value, preferred)
}

function onKeydown(event: KeyboardEvent): void {
  if (!isOpen.value) return
  if (event.key === 'Escape' && props.closeOnEscape && canClose.value) {
    event.preventDefault()
    requestClose()
    return
  }
  if (event.key !== 'Tab') return
  const focusList = focusables()
  if (focusList.length === 0) {
    event.preventDefault()
    dialogRef.value?.focus()
    return
  }
  const first = focusList[0]
  const last = focusList[focusList.length - 1]
  if (!first || !last) return
  const active = document.activeElement as HTMLElement | null
  if (event.shiftKey) {
    if (active === first || !dialogRef.value?.contains(active)) {
      event.preventDefault()
      last.focus()
    }
  } else {
    if (active === last) {
      event.preventDefault()
      first.focus()
    }
  }
}

function requestClose(): void {
  if (!canClose.value) return
  emit('update:open', false)
  emit('close')
}

function onPrimary(): void {
  emit('primary')
}

function onSecondary(): void {
  if (!canClose.value) return
  emit('secondary')
  if (props.closeOnSecondary) requestClose()
}

function onBackdrop(): void {
  if (props.closeOnBackdrop && canClose.value) {
    requestClose()
  }
}

watch(
  isOpen,
  async (value) => {
    if (value) {
      if (typeof document !== 'undefined') {
        returnFocus.value = (document.activeElement as HTMLElement | null) ?? null
        lock()
      }
      await nextTick()
      focusInitial()
    } else {
      if (typeof document !== 'undefined') {
        unlock()
        if (returnFocus.value && document.contains(returnFocus.value)) {
          returnFocus.value.focus()
        }
        returnFocus.value = null
      }
    }
  },
  { immediate: true },
)
</script>

<template>
  <Teleport to="body">
    <div
      v-if="isOpen"
      class="app-dialog__backdrop"
      @mousedown.self="onBackdrop"
      :aria-busy="isPending || undefined"
      data-testid="app-dialog"
    >
      <div
        ref="dialogRef"
        class="app-dialog"
        role="dialog"
        aria-modal="true"
        :aria-labelledby="titleId"
        :aria-describedby="description ? descriptionId : undefined"
        tabindex="-1"
        @keydown="onKeydown"
      >
        <header class="app-dialog__header">
          <div class="app-dialog__heading">
            <h2 :id="titleId" class="app-dialog__title">{{ title }}</h2>
            <p v-if="description" :id="descriptionId" class="app-dialog__desc">{{ description }}</p>
          </div>
          <button
            ref="closeBtnRef"
            type="button"
            class="app-dialog__close"
            :aria-label="ariaLabelClose"
            :disabled="!canClose"
            @click="requestClose"
          >
            <X :size="16" aria-hidden="true" />
          </button>
        </header>
        <div class="app-dialog__body">
          <slot />
        </div>
        <footer class="app-dialog__footer">
          <AppButton
            v-if="showSecondary"
            ref="secondaryRef"
            variant="ghost"
            :disabled="isPending"
            @click="onSecondary"
          >
            {{ secondaryLabel }}
          </AppButton>
          <AppButton
            ref="primaryRef"
            :variant="primaryVariant"
            :loading="primaryLoading"
            :disabled="primaryDisabled"
            @click="onPrimary"
          >
            {{ primaryLabel }}
          </AppButton>
        </footer>
      </div>
    </div>
  </Teleport>
</template>

<style scoped>
.app-dialog__backdrop {
  position: fixed;
  inset: 0;
  background: var(--pd-overlay-bg);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: var(--pd-z-overlay);
  padding: var(--pd-space-16);
}

.app-dialog {
  width: min(100%, var(--pd-dialog-max-width));
  max-width: var(--pd-dialog-max-width);
  max-height: 90dvh;
  display: flex;
  flex-direction: column;
  background: var(--pd-container-workspace-bg);
  color: var(--pd-text-default);
  border: 1px solid var(--pd-border-overlay);
  border-radius: var(--pd-radius-lg);
  box-shadow: var(--pd-shadow-lg);
  outline: none;
}

.app-dialog__header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--pd-space-16);
  padding: var(--pd-space-16) var(--pd-space-16) var(--pd-space-8);
}

.app-dialog__heading {
  min-width: 0;
}

.app-dialog__title {
  font-size: var(--pd-font-size-14);
  font-weight: var(--pd-font-weight-semibold);
  line-height: var(--pd-line-height-compact);
}

.app-dialog__desc {
  margin-top: var(--pd-space-4);
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-12);
  line-height: var(--pd-line-height-metadata);
}

.app-dialog__close {
  width: var(--pd-icon-button-size-sm);
  height: var(--pd-icon-button-size-sm);
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: var(--pd-radius-sm);
  color: var(--pd-text-muted);
  background: transparent;
  border: 1px solid transparent;
  transition:
    background var(--pd-transition-fast),
    color var(--pd-transition-fast);
}

.app-dialog__close:hover:not(:disabled) {
  background: var(--pd-control-bg-hovered);
  color: var(--pd-text-default);
}

.app-dialog__close:disabled {
  cursor: not-allowed;
  color: var(--pd-text-disabled);
}

.app-dialog__body {
  padding: 0 var(--pd-space-16) var(--pd-space-16);
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: var(--pd-space-16);
}

.app-dialog__footer {
  display: flex;
  justify-content: flex-end;
  gap: var(--pd-space-8);
  padding: var(--pd-space-16);
  border-top: 1px solid var(--pd-border-separator);
}

@media (max-width: 767px) {
  .app-dialog__footer {
    flex-direction: column;
  }
}

@media (max-width: 767px), (pointer: coarse) {
  .app-dialog__close {
    width: var(--pd-control-height-touch);
    height: var(--pd-control-height-touch);
  }
}
</style>
