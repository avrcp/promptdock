import { nextTick, onBeforeUnmount, ref } from 'vue'

export function useFocusReturn() {
  const previous = ref<HTMLElement | null>(null)

  function capture(): void {
    previous.value = document.activeElement instanceof HTMLElement ? document.activeElement : null
  }

  async function restore(): Promise<void> {
    await nextTick()
    if (previous.value && document.contains(previous.value)) previous.value.focus()
    previous.value = null
  }

  onBeforeUnmount(() => {
    previous.value = null
  })

  return { capture, restore }
}
