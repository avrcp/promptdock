<script setup lang="ts">
import AppMenu, { type AppMenuItem } from '@/components/AppMenu.vue'
import AppTable from '@/components/AppTable.vue'
import StatusChip from '@/components/StatusChip.vue'
import TimeAgo from '@/components/TimeAgo.vue'
import type { DeviceListItem } from '@/contracts/device'
import {
  canEnable,
  deviceStateVisual,
  gatewayStateVisual,
  isDeviceActionable,
  shortDeviceId,
} from './device-status'
import type { DeviceMutationAction } from './useDevicesController'

const props = defineProps<{
  rows: DeviceListItem[]
  refreshing: boolean
  canManage: boolean
  manageReason: string | null
}>()

const emit = defineEmits<{
  open: [id: string]
  mutate: [row: DeviceListItem, action: DeviceMutationAction]
}>()

const columns = [
  { key: 'device', label: '设备', width: '24%' },
  { key: 'state', label: '状态', width: '112px' },
  { key: 'gateway', label: 'Gateway', width: '128px' },
  { key: 'scopes', label: 'Scopes' },
  { key: 'tokenFormat', label: 'Token', width: '88px' },
  { key: 'lastSeen', label: '最后在线', width: '112px' },
  { key: 'actions', label: '操作', width: '48px', align: 'right' as const },
]

function visibleScopes(scopes: string[]): string {
  return scopes.length <= 3
    ? scopes.join(' · ')
    : `${scopes.slice(0, 3).join(' · ')} +${scopes.length - 3}`
}

function actionDisabledReason(row: DeviceListItem): string | undefined {
  if (!props.canManage) return props.manageReason ?? '当前环境不允许管理设备。'
  if (!isDeviceActionable(row)) return '该设备当前状态不支持此操作。'
  return undefined
}

function toggleAction(row: DeviceListItem): { action: DeviceMutationAction; label: string } {
  if (canEnable(row)) return { action: 'enable', label: '启用设备' }
  return { action: 'disable', label: '禁用设备' }
}

function menuItems(row: DeviceListItem): AppMenuItem[] {
  const disabledReason = actionDisabledReason(row)
  const toggle = toggleAction(row)
  return [
    { id: 'details', label: '查看详情' },
    { id: 'rotate', label: 'Rotate Token', disabled: Boolean(disabledReason), disabledReason },
    { id: toggle.action, label: toggle.label, disabled: Boolean(disabledReason), disabledReason },
    {
      id: 'revoke',
      label: '撤销设备',
      tone: 'danger',
      separatorBefore: true,
      disabled: Boolean(disabledReason),
      disabledReason,
    },
  ]
}

function onMenuSelect(row: DeviceListItem, item: AppMenuItem): void {
  if (item.id === 'details') {
    emit('open', row.id)
    return
  }
  emit('mutate', row, item.id as DeviceMutationAction)
}
</script>

<template>
  <AppTable
    class="device-table__desktop"
    :columns="columns"
    :rows="rows"
    :row-key="(row: DeviceListItem) => row.id"
    :interactive-rows="false"
    :loading="refreshing"
    :aria-label="`设备列表（${rows.length} 条）`"
  >
    <template #cell-device="{ row }">
      <div class="device-table__identity">
        <button
          type="button"
          class="device-table__details"
          :aria-label="`查看设备详情：${row.name}`"
          :title="row.name"
          data-testid="device-details"
          @click="emit('open', row.id)"
        >
          {{ row.name }}
        </button>
        <span class="device-table__id mono break-anywhere">{{ shortDeviceId(row.id) }}</span>
      </div>
    </template>
    <template #cell-state="{ row }">
      <StatusChip
        :tone="deviceStateVisual(row.state).tone"
        :label="deviceStateVisual(row.state).label"
      />
    </template>
    <template #cell-gateway="{ row }">
      <StatusChip
        :tone="gatewayStateVisual(row.gatewayState).tone"
        :label="gatewayStateVisual(row.gatewayState).label"
      />
    </template>
    <template #cell-scopes="{ row }">
      <span class="device-table__scopes">{{ visibleScopes(row.scopes) }}</span>
    </template>
    <template #cell-tokenFormat="{ row }">
      <span class="device-table__token mono break-anywhere">pdv{{ row.tokenFormatVersion }}</span>
    </template>
    <template #cell-lastSeen="{ row }"><TimeAgo :timestamp="row.lastSeenAt" /></template>
    <template #cell-actions="{ row }">
      <div class="device-table__actions" @click.stop>
        <AppMenu
          :items="menuItems(row)"
          :trigger-label="`设备操作：${row.name}`"
          :aria-label="`${row.name} 的操作`"
          data-testid="device-actions"
          @select="onMenuSelect(row, $event)"
        />
      </div>
    </template>
  </AppTable>

  <ul class="device-table__mobile" aria-label="设备列表">
    <li v-for="row in rows" :key="row.id" class="device-table__mobile-card">
      <div class="device-table__mobile-heading">
        <div class="device-table__identity">
          <button
            type="button"
            class="device-table__details"
            :aria-label="`查看设备详情：${row.name}`"
            :title="row.name"
            data-testid="device-details-mobile"
            @click="emit('open', row.id)"
          >
            {{ row.name }}
          </button>
          <span class="device-table__id mono break-anywhere">{{ shortDeviceId(row.id) }}</span>
        </div>
        <AppMenu
          :items="menuItems(row)"
          :trigger-label="`设备操作：${row.name}`"
          :aria-label="`${row.name} 的操作`"
          @select="onMenuSelect(row, $event)"
        />
      </div>
      <div class="device-table__mobile-states">
        <StatusChip
          :tone="deviceStateVisual(row.state).tone"
          :label="deviceStateVisual(row.state).label"
        />
        <StatusChip
          :tone="gatewayStateVisual(row.gatewayState).tone"
          :label="gatewayStateVisual(row.gatewayState).label"
        />
      </div>
      <dl class="device-table__mobile-meta">
        <div>
          <dt>Scopes</dt>
          <dd>{{ visibleScopes(row.scopes) }}</dd>
        </div>
        <div>
          <dt>Token</dt>
          <dd class="mono break-anywhere">pdv{{ row.tokenFormatVersion }}</dd>
        </div>
        <div>
          <dt>最后在线</dt>
          <dd><TimeAgo :timestamp="row.lastSeenAt" /></dd>
        </div>
      </dl>
    </li>
  </ul>
</template>

<style scoped>
.device-table__mobile {
  display: none;
}
.device-table__identity {
  display: flex;
  flex-direction: column;
  gap: var(--pd-space-2);
  min-width: 0;
}
.device-table__details {
  width: fit-content;
  max-width: 100%;
  padding: 0;
  overflow: hidden;
  color: var(--pd-text-default);
  font: inherit;
  font-weight: var(--pd-font-weight-medium);
  text-align: start;
  text-overflow: ellipsis;
  white-space: nowrap;
  background: transparent;
  border: 0;
  cursor: pointer;
}
.device-table__details:hover {
  color: var(--pd-text-link);
  text-decoration: underline;
}
.device-table__id,
.device-table__token {
  color: var(--pd-text-subtle);
  font-size: var(--pd-font-size-11);
  line-height: var(--pd-line-height-metadata);
}
.device-table__scopes {
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-12);
  line-height: var(--pd-line-height-metadata);
  overflow-wrap: normal;
  word-break: keep-all;
}
.device-table__actions {
  display: inline-flex;
  justify-content: flex-end;
}
@media (max-width: 767px) {
  .device-table__desktop {
    display: none;
  }
  .device-table__mobile {
    display: flex;
    flex-direction: column;
    gap: var(--pd-space-8);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .device-table__mobile-card {
    display: flex;
    flex-direction: column;
    gap: var(--pd-space-12);
    padding: var(--pd-space-12);
    background: var(--pd-container-panel-bg);
    border: 1px solid var(--pd-border-separator);
    border-radius: var(--pd-radius-md);
  }
  .device-table__mobile-heading,
  .device-table__mobile-states {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--pd-space-12);
  }
  .device-table__mobile-states {
    justify-content: flex-start;
    flex-wrap: wrap;
  }
  .device-table__mobile-meta {
    display: flex;
    flex-direction: column;
    gap: var(--pd-space-8);
    margin: 0;
  }
  .device-table__mobile-meta div {
    display: flex;
    justify-content: space-between;
    gap: var(--pd-space-12);
    font-size: var(--pd-font-size-12);
    line-height: var(--pd-line-height-metadata);
  }
  .device-table__mobile-meta dt {
    color: var(--pd-text-muted);
  }
  .device-table__mobile-meta dd {
    min-width: 0;
    margin: 0;
    color: var(--pd-text-default);
    text-align: end;
    overflow-wrap: anywhere;
  }
}
</style>
