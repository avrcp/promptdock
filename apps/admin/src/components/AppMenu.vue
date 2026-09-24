<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, useId, watch } from 'vue'
import { MoreHorizontal } from 'lucide-vue-next'

export interface AppMenuItem {
  id: string
  label: string
  disabled?: boolean
  disabledReason?: string
  tone?: 'default' | 'danger'
  separatorBefore?: boolean
}

interface AppMenuProps {
  items: readonly AppMenuItem[]
  ariaLabel?: string
  triggerLabel?: string
  align?: 'start' | 'end'
}

const props = withDefaults(defineProps<AppMenuProps>(), {
  ariaLabel: '操作菜单',
  triggerLabel: '更多操作',
  align: 'end',
})

const emit = defineEmits<{
  (e: 'select', item: AppMenuItem): void
}>()

const open = ref(false)
const triggerRef = ref<HTMLButtonElement | null>(null)
const menuRef = ref<HTMLDivElement | null>(null)
const itemRefs = ref<HTMLButtonElement[]>([])
const menuId = useId()
const position = ref({ left: 0, top: 0 })
const menuStyle = computed(() => ({
  left: `${position.value.left}px`,
  top: `${position.value.top}px`,
}))

function menuButtons(): HTMLButtonElement[] {
  return itemRefs.value
}

function focusItem(index: number): void {
  menuButtons()[index]?.focus()
}

function reasonId(item: AppMenuItem): string {
  return `${menuId}-reason-${item.id}`
}

function updatePosition(): void {
  const trigger = triggerRef.value
  const menu = menuRef.value
  if (!trigger || !menu || typeof window === 'undefined') return

  const triggerRect = trigger.getBoundingClientRect()
  const menuRect = menu.getBoundingClientRect()
  const edge = 8
  const preferredLeft =
    props.align === 'end' ? triggerRect.right - menuRect.width : triggerRect.left
  const left = Math.max(edge, Math.min(preferredLeft, window.innerWidth - menuRect.width - edge))
  const below = triggerRect.bottom + 4
  const above = triggerRect.top - menuRect.height - 4
  const top = below + menuRect.height <= window.innerHeight - edge || above < edge ? below : above
  position.value = { left, top }
}

async function openMenu(focus: 'first' | 'last' | null = null): Promise<void> {
  open.value = true
  await nextTick()
  updatePosition()
  if (focus === 'first') focusItem(0)
  if (focus === 'last') focusItem(menuButtons().length - 1)
}

function closeMenu(restoreFocus = true): void {
  if (!open.value) return
  open.value = false
  if (restoreFocus) triggerRef.value?.focus()
}

function toggleMenu(): void {
  if (open.value) closeMenu()
  else void openMenu()
}

function onTriggerKeydown(event: KeyboardEvent): void {
  if (event.key === 'ArrowDown') {
    event.preventDefault()
    void openMenu('first')
  } else if (event.key === 'ArrowUp') {
    event.preventDefault()
    void openMenu('last')
  }
}

function onMenuKeydown(event: KeyboardEvent): void {
  const items = menuButtons()
  if (event.key === 'Escape') {
    event.preventDefault()
    closeMenu()
    return
  }
  if (items.length === 0) return

  const currentIndex = items.indexOf(document.activeElement as HTMLButtonElement)
  if (event.key === 'ArrowDown') {
    event.preventDefault()
    focusItem((currentIndex + 1 + items.length) % items.length)
  } else if (event.key === 'ArrowUp') {
    event.preventDefault()
    focusItem((currentIndex - 1 + items.length) % items.length)
  } else if (event.key === 'Home') {
    event.preventDefault()
    focusItem(0)
  } else if (event.key === 'End') {
    event.preventDefault()
    focusItem(items.length - 1)
  }
}

function select(item: AppMenuItem): void {
  if (item.disabled) return
  emit('select', item)
  closeMenu()
}

function onDocumentPointerdown(event: PointerEvent): void {
  const target = event.target
  if (!(target instanceof Node)) return
  if (!menuRef.value?.contains(target) && !triggerRef.value?.contains(target)) closeMenu()
}

watch(open, (value) => {
  if (typeof window === 'undefined') return
  if (value) {
    document.addEventListener('pointerdown', onDocumentPointerdown)
    window.addEventListener('resize', updatePosition)
    window.addEventListener('scroll', updatePosition, true)
  } else {
    document.removeEventListener('pointerdown', onDocumentPointerdown)
    window.removeEventListener('resize', updatePosition)
    window.removeEventListener('scroll', updatePosition, true)
  }
})

onBeforeUnmount(() => {
  document.removeEventListener('pointerdown', onDocumentPointerdown)
  window.removeEventListener('resize', updatePosition)
  window.removeEventListener('scroll', updatePosition, true)
})
</script>

<template>
  <span class="app-menu" data-testid="app-menu">
    <button
      ref="triggerRef"
      type="button"
      class="app-menu__trigger"
      :aria-label="triggerLabel"
      aria-haspopup="menu"
      :aria-controls="open ? menuId : undefined"
      :aria-expanded="open"
      data-testid="app-menu-trigger"
      @click="toggleMenu"
      @keydown="onTriggerKeydown"
    >
      <slot name="trigger" :open="open">
        <MoreHorizontal :size="16" aria-hidden="true" />
      </slot>
    </button>
    <Teleport to="body">
      <div
        v-if="open"
        :id="menuId"
        ref="menuRef"
        class="app-menu__popup"
        role="menu"
        :aria-label="ariaLabel"
        :style="menuStyle"
        data-testid="app-menu-popup"
        @keydown="onMenuKeydown"
      >
        <template v-for="item in items" :key="item.id">
          <div v-if="item.separatorBefore" class="app-menu__separator" role="separator" />
          <button
            ref="itemRefs"
            type="button"
            class="app-menu__item"
            :class="{ 'app-menu__item--danger': item.tone === 'danger' }"
            role="menuitem"
            :aria-disabled="item.disabled || undefined"
            :aria-describedby="item.disabledReason ? reasonId(item) : undefined"
            @click="select(item)"
          >
            <span class="app-menu__label">{{ item.label }}</span>
            <span v-if="item.disabledReason" :id="reasonId(item)" class="app-menu__reason">
              {{ item.disabledReason }}
            </span>
          </button>
        </template>
      </div>
    </Teleport>
  </span>
</template>

<style scoped>
.app-menu {
  display: inline-flex;
}

.app-menu__trigger {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: var(--pd-icon-button-size-sm);
  height: var(--pd-icon-button-size-sm);
  color: var(--pd-text-muted);
  background: transparent;
  border: 1px solid transparent;
  border-radius: var(--pd-radius-sm);
  cursor: pointer;
  transition:
    background var(--pd-transition-fast),
    color var(--pd-transition-fast);
}

.app-menu__trigger:hover,
.app-menu__trigger[aria-expanded='true'] {
  color: var(--pd-text-default);
  background: var(--pd-control-bg-hovered);
}

.app-menu__popup {
  position: fixed;
  z-index: var(--pd-z-menu);
  display: flex;
  flex-direction: column;
  width: max-content;
  min-width: 160px;
  max-width: 240px;
  padding: var(--pd-space-4);
  background: var(--pd-container-panel-bg);
  border: 1px solid var(--pd-border-overlay);
  border-radius: var(--pd-radius-sm);
  box-shadow: var(--pd-shadow-sm);
}

.app-menu__item {
  display: flex;
  flex-direction: column;
  justify-content: center;
  gap: var(--pd-space-2);
  width: 100%;
  min-height: var(--pd-control-height-sm);
  padding: 0 var(--pd-space-8);
  overflow: hidden;
  color: var(--pd-text-default);
  font-size: var(--pd-font-size-13);
  line-height: var(--pd-line-height-ui);
  text-align: start;
  text-overflow: ellipsis;
  white-space: nowrap;
  background: transparent;
  border: 0;
  border-radius: var(--pd-radius-xs);
  cursor: pointer;
}

.app-menu__item[aria-disabled='true'] {
  color: var(--pd-control-disabled-text);
  cursor: not-allowed;
}

.app-menu__label {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.app-menu__reason {
  color: var(--pd-text-subtle);
  font-size: var(--pd-font-size-11);
  line-height: var(--pd-line-height-metadata);
  white-space: normal;
  overflow-wrap: anywhere;
}

.app-menu__item:hover,
.app-menu__item:focus-visible {
  background: var(--pd-control-bg-hovered);
}

.app-menu__item--danger {
  color: var(--pd-feedback-danger);
}

.app-menu__separator {
  height: 1px;
  margin: var(--pd-space-4) 0;
  background: var(--pd-border-separator);
}

@media (max-width: 767px), (pointer: coarse) {
  .app-menu__trigger {
    width: var(--pd-control-height-touch);
    height: var(--pd-control-height-touch);
  }
}
</style>
