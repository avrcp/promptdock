<script setup lang="ts">
import { computed, nextTick, ref, useId, watch } from 'vue'
import { X } from 'lucide-vue-next'

import { useOverlayScrollLock } from '@/composables/useOverlayScrollLock'
import { FOCUSABLE_SELECTOR, focusPreferredOrFirst } from '@/utils/focus-management'

interface AppDrawerProps {
  open: boolean
  title: string
  width?: number
  pending?: boolean
  closable?: boolean
  closeOnBackdrop?: boolean
  closeOnEscape?: boolean
  ariaLabelClose?: string
}

const props = withDefaults(defineProps<AppDrawerProps>(), {
  width: 420,
  pending: false,
  closable: true,
  closeOnBackdrop: true,
  closeOnEscape: true,
  ariaLabelClose: '关闭抽屉',
})

const emit = defineEmits<{
  (e: 'update:open', value: boolean): void
  (e: 'close'): void
}>()

const drawerRef = ref<HTMLDivElement | null>(null)
const closeBtnRef = ref<HTMLButtonElement | null>(null)
const returnFocus = ref<HTMLElement | null>(null)
const titleId = useId()
const { lock, unlock } = useOverlayScrollLock()

const isOpen = computed(() => props.open)
const canClose = computed(() => props.closable && !props.pending)
const resolvedWidth = computed(() => props.width)

function focusables(): HTMLElement[] {
  if (!drawerRef.value) return []
  return Array.from(drawerRef.value.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR))
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
    drawerRef.value?.focus()
    return
  }
  const first = focusList[0]
  const last = focusList[focusList.length - 1]
  if (!first || !last) return
  const active = document.activeElement as HTMLElement | null
  if (event.shiftKey) {
    if (active === first || !drawerRef.value?.contains(active)) {
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

watch(
  isOpen,
  async (value) => {
    if (value) {
      if (typeof document !== 'undefined') {
        returnFocus.value = (document.activeElement as HTMLElement | null) ?? null
        lock()
      }
      await nextTick()
      focusPreferredOrFirst(drawerRef.value, closeBtnRef.value)
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
      class="app-drawer__backdrop"
      @mousedown.self="closeOnBackdrop && requestClose()"
      data-testid="app-drawer"
    >
      <aside
        ref="drawerRef"
        class="app-drawer"
        role="dialog"
        aria-modal="true"
        :aria-labelledby="titleId"
        :aria-busy="pending || undefined"
        :style="{ '--app-drawer-width': resolvedWidth + 'px' }"
        tabindex="-1"
        @keydown="onKeydown"
      >
        <header class="app-drawer__header">
          <h2 :id="titleId" class="app-drawer__title" :title="title">{{ title }}</h2>
          <button
            ref="closeBtnRef"
            type="button"
            class="app-drawer__close"
            :aria-label="ariaLabelClose"
            :disabled="!canClose"
            @click="requestClose"
          >
            <X :size="16" aria-hidden="true" />
          </button>
        </header>
        <div class="app-drawer__body">
          <slot />
        </div>
        <footer v-if="$slots.footer" class="app-drawer__footer">
          <slot name="footer" />
        </footer>
      </aside>
    </div>
  </Teleport>
</template>

<style scoped>
.app-drawer__backdrop {
  position: fixed;
  inset: 0;
  background: var(--pd-overlay-bg);
  display: flex;
  justify-content: flex-end;
  z-index: var(--pd-z-overlay);
}

.app-drawer {
  width: min(var(--app-drawer-width), calc(100vw - var(--pd-space-24)));
  background: var(--pd-container-workspace-bg);
  color: var(--pd-text-default);
  border-inline-start: 1px solid var(--pd-border-overlay);
  box-shadow: var(--pd-shadow-lg);
  height: 100%;
  max-width: 100vw;
  display: flex;
  flex-direction: column;
  outline: none;
  transition: transform var(--pd-transition-normal);
}

.app-drawer__header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  min-height: var(--pd-shell-topbar-height);
  padding: 0 var(--pd-space-16);
  border-bottom: 1px solid var(--pd-border-separator);
  gap: var(--pd-space-12);
}

.app-drawer__title {
  font-size: var(--pd-font-size-14);
  font-weight: var(--pd-font-weight-semibold);
  line-height: var(--pd-line-height-compact);
  flex: 1 1 auto;
  min-width: 0;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.app-drawer__close {
  width: var(--pd-icon-button-size-sm);
  height: var(--pd-icon-button-size-sm);
  flex: 0 0 auto;
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

.app-drawer__close:hover:not(:disabled) {
  background: var(--pd-control-bg-hovered);
  color: var(--pd-text-default);
}

.app-drawer__close:disabled {
  cursor: not-allowed;
  color: var(--pd-text-disabled);
}

.app-drawer__body {
  padding: var(--pd-space-16);
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: var(--pd-space-16);
  flex: 1;
  min-height: 0;
}

.app-drawer__footer {
  position: sticky;
  bottom: 0;
  padding: var(--pd-space-12) var(--pd-space-16);
  background: var(--pd-container-workspace-bg);
  border-top: 1px solid var(--pd-border-separator);
  display: flex;
  justify-content: flex-end;
  gap: var(--pd-space-8);
}

@media (max-width: 767px) {
  .app-drawer {
    width: 100vw;
    max-width: 100vw;
  }
}

@media (max-width: 767px), (pointer: coarse) {
  .app-drawer__close {
    width: var(--pd-control-height-touch);
    height: var(--pd-control-height-touch);
  }
}
</style>
