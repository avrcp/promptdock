<script setup lang="ts">
import { AlertTriangle, Plus, RefreshCw } from 'lucide-vue-next'

import AdminErrorAlert from '@/components/AdminErrorAlert.vue'
import AppButton from '@/components/AppButton.vue'
import CredentialReceiptDialog from '@/components/CredentialReceiptDialog.vue'
import EmptyState from '@/components/EmptyState.vue'
import SkeletonBlock from '@/components/SkeletonBlock.vue'
import DeviceCreateDialog from './DeviceCreateDialog.vue'
import DeviceDetailDrawer from './DeviceDetailDrawer.vue'
import DeviceFilters from './DeviceFilters.vue'
import DeviceMutationDialog from './DeviceMutationDialog.vue'
import DeviceTable from './DeviceTable.vue'
import { useDevicesController } from './useDevicesController'

const controller = useDevicesController()
const {
  stateFilter,
  searchInput,
  listResource,
  devices,
  loading,
  drawerOpen,
  detailResource,
  selectedDetail,
  detailLoading,
  createDialogOpen,
  createName,
  createScopes,
  createInFlight,
  createError,
  mutationAction,
  mutationDevice,
  mutationInFlight,
  mutationError,
  receiptState,
  receiptDialogOpen,
  receiptActionHint,
  canManage,
  manageReason,
  openCreate,
  submitCreate,
  openMutation,
  closeMutation,
  submitMutation,
  clearCredentialReceipt,
  clearFilters,
  refresh,
  openDevice,
} = controller
</script>

<template>
  <div class="devices-page">
    <p
      v-if="manageReason"
      id="device-manage-capability-reason"
      class="devices-page__capability-hint"
      data-testid="device-capability-reason"
    >
      {{ manageReason }}
    </p>

    <DeviceFilters
      v-model:state="stateFilter"
      v-model:search="searchInput"
      :result-count="devices.length"
      :has-active-filter="Boolean(stateFilter || searchInput)"
      @clear="clearFilters"
    >
      <template #actions>
        <AppButton
          variant="secondary"
          size="sm"
          :loading="listResource.refreshing.value"
          @click="refresh"
        >
          <template #default>
            <RefreshCw :size="14" aria-hidden="true" />
            <span>刷新</span>
          </template>
        </AppButton>
        <AppButton
          variant="primary"
          size="sm"
          data-testid="device-create"
          :disabled="!canManage"
          :aria-describedby="manageReason ? 'device-manage-capability-reason' : undefined"
          @click="openCreate"
        >
          <template #default>
            <Plus :size="14" aria-hidden="true" />
            <span>新建设备</span>
          </template>
        </AppButton>
      </template>
    </DeviceFilters>

    <AdminErrorAlert
      v-if="listResource.error.value"
      :error="listResource.error.value"
      testid="devices-error"
      @retry="refresh"
    />
    <div
      v-if="loading"
      class="devices-page__skeleton"
      data-testid="devices-skeleton"
      role="status"
      aria-live="polite"
    >
      <span class="sr-only">正在加载设备列表</span>
      <SkeletonBlock v-for="index in 4" :key="index" variant="table-row" />
    </div>
    <div
      v-else-if="listResource.error.value && listResource.data.value === null"
      class="devices-page__initial-error"
      data-testid="devices-initial-error"
    >
      当前无法确认设备列表是否为空，请使用上方恢复操作重试。
    </div>
    <EmptyState
      v-else-if="listResource.data.value !== null && devices.length === 0"
      :icon="AlertTriangle"
      title="没有匹配的设备"
      :description="
        stateFilter || searchInput
          ? '当前筛选条件下没有设备。尝试调整状态或清空搜索。'
          : '当前还没有任何设备。'
      "
    >
      <template v-if="stateFilter || searchInput" #action>
        <AppButton variant="secondary" size="sm" @click="clearFilters">清空筛选</AppButton>
      </template>
    </EmptyState>
    <DeviceTable
      v-else
      :rows="devices"
      :refreshing="listResource.refreshing.value"
      :can-manage="canManage"
      :manage-reason="manageReason"
      @open="openDevice"
      @mutate="openMutation"
    />

    <DeviceCreateDialog
      v-model:open="createDialogOpen"
      v-model:name="createName"
      v-model:scopes="createScopes"
      :in-flight="createInFlight"
      :error="createError"
      :can-manage="canManage"
      @submit="submitCreate"
    />
    <DeviceMutationDialog
      :action="mutationAction"
      :device="mutationDevice"
      :in-flight="mutationInFlight"
      :error="mutationError"
      :can-manage="canManage"
      @close="closeMutation"
      @submit="submitMutation"
    />
    <DeviceDetailDrawer
      v-model:open="drawerOpen"
      :detail="selectedDetail"
      :loading="detailLoading"
      :error="detailResource.error.value"
      @retry="detailResource.refresh('refresh')"
    />
    <CredentialReceiptDialog
      v-model:open="receiptDialogOpen"
      :receipt="receiptState?.receipt ?? null"
      :device-name="receiptState?.deviceName ?? ''"
      :action-label="receiptState?.receipt.action === 'rotate' ? 'Rotate Token' : '新建设备'"
      :action-hint="receiptActionHint"
      @close="clearCredentialReceipt"
    />
  </div>
</template>

<style scoped>
.devices-page__capability-hint,
.devices-page__initial-error {
  margin-bottom: var(--pd-space-16);
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-13);
  line-height: var(--pd-line-height-ui);
}

.devices-page__skeleton {
  display: flex;
  flex-direction: column;
  gap: var(--pd-space-8);
}
</style>
