<script setup lang="ts">
import { computed } from 'vue'

import AppDialog from '@/components/AppDialog.vue'
import InlineAlert from '@/components/InlineAlert.vue'
import type { DeviceListItem } from '@/contracts/device'
import type { AdminError } from '@/contracts/error'
import type { DeviceMutationAction } from './useDevicesController'

const props = defineProps<{
  action: DeviceMutationAction | null
  device: DeviceListItem | null
  inFlight: boolean
  error: AdminError | null
  canManage: boolean
}>()

const emit = defineEmits<{ close: []; submit: [] }>()

const title = computed(() => {
  if (!props.action || !props.device) return ''
  const labels = {
    rotate: 'Rotate 设备',
    enable: '启用设备',
    disable: '禁用设备',
    revoke: '撤销设备',
  }
  return `${labels[props.action]}：${props.device.name}`
})
const description = computed(() => {
  if (props.action === 'rotate')
    return 'Rotate 会立即失效当前 token 并生成一次性新 token。请在确认前确保你知道这个动作会让当前设备掉线。'
  if (props.action === 'enable') return '启用后该设备将恢复出站能力。'
  if (props.action === 'disable')
    return '禁用后该设备暂时失去出站能力，但凭证仍然有效，可再次启用。'
  if (props.action === 'revoke')
    return '撤销是不可逆动作：设备将永久失去出站能力，所有未发出的 outbox 通知会被标记为不可送达。'
  return ''
})
const primaryLabel = computed(() => {
  if (props.action === 'rotate') return 'Rotate Token'
  if (props.action === 'enable') return '启用'
  if (props.action === 'disable') return '禁用'
  if (props.action === 'revoke') return '撤销'
  return '确认'
})
const primaryVariant = computed<'primary' | 'danger' | 'secondary'>(() =>
  props.action === 'revoke' ? 'danger' : props.action === 'disable' ? 'secondary' : 'primary',
)
</script>

<template>
  <AppDialog
    :open="action !== null"
    :title="title"
    :description="description"
    :primary-label="primaryLabel"
    :primary-variant="primaryVariant"
    :primary-loading="inFlight"
    :primary-disabled="!canManage"
    :pending="inFlight"
    @update:open="(value: boolean) => value || emit('close')"
    @primary="emit('submit')"
    @secondary="emit('close')"
  >
    <InlineAlert v-if="error" tone="danger" :title="error.message" :code="error.code" />
    <p v-else class="device-mutation__hint">请确认操作影响后再继续。</p>
  </AppDialog>
</template>

<style scoped>
.device-mutation__hint {
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-13);
  line-height: var(--pd-line-height-ui);
}
</style>
