<script setup lang="ts">
import QRCode from 'qrcode'
import { onBeforeUnmount, ref, watch } from 'vue'

const props = defineProps<{
  /** Sensitive QR payload: never render this string or expose it as an attribute. */
  content: string | null
}>()

const host = ref<HTMLDivElement | null>(null)
let epoch = 0

function clearQr(): void {
  host.value?.replaceChildren()
}

async function render(content: string | null): Promise<void> {
  const renderEpoch = ++epoch
  clearQr()
  if (!content) return
  try {
    const svg = await QRCode.toString(content, {
      type: 'svg',
      errorCorrectionLevel: 'M',
      margin: 1,
    })
    if (renderEpoch !== epoch) return
    const documentNode = new DOMParser().parseFromString(svg, 'image/svg+xml')
    const svgNode = documentNode.documentElement
    if (svgNode.localName !== 'svg' || documentNode.querySelector('parsererror')) {
      clearQr()
      return
    }
    host.value?.replaceChildren(document.importNode(svgNode, true))
  } catch {
    // The parent supplies a generic error state; never reflect the QR payload.
    clearQr()
  }
}

watch(() => props.content, render, { immediate: true })
onBeforeUnmount(() => {
  epoch += 1
  clearQr()
})
</script>

<template>
  <div
    ref="host"
    class="wechat-qr-canvas"
    role="img"
    aria-label="微信登录二维码（仅在当前页面内存中呈现）"
  />
</template>

<style scoped>
.wechat-qr-canvas {
  width: 192px;
  height: 192px;
  background: var(--pd-qr-surface-bg);
  border-radius: var(--pd-radius-sm);
}

.wechat-qr-canvas :deep(svg) {
  display: block;
  width: 100%;
  height: 100%;
}
</style>
