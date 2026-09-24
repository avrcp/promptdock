<script setup lang="ts">
import AdminErrorAlert from '@/components/AdminErrorAlert.vue'
import AppDrawer from '@/components/AppDrawer.vue'
import CopyValue from '@/components/CopyValue.vue'
import EmptyState from '@/components/EmptyState.vue'
import SkeletonBlock from '@/components/SkeletonBlock.vue'
import StatusChip from '@/components/StatusChip.vue'
import TimeAgo from '@/components/TimeAgo.vue'
import type { DeviceDetail } from '@/contracts/device'
import type { AdminError } from '@/contracts/error'
import { deviceStateVisual, gatewayStateVisual, shortDeviceId } from './device-status'

defineProps<{
  detail: DeviceDetail | null
  loading: boolean
  error: AdminError | null
}>()

const open = defineModel<boolean>('open', { required: true })
const emit = defineEmits<{ retry: [] }>()
</script>

<template>
  <AppDrawer v-model:open="open" :title="detail?.name ?? '设备详情'" :width="420">
    <template v-if="loading">
      <div role="status" aria-live="polite">
        <span class="sr-only">正在加载设备详情</span>
        <SkeletonBlock variant="text" width="60%" />
        <SkeletonBlock variant="panel" />
      </div>
    </template>
    <AdminErrorAlert v-else-if="error" :error="error" @retry="emit('retry')" />
    <template v-else-if="detail">
      <section aria-labelledby="device-detail-state">
        <h3 id="device-detail-state" class="device-detail__heading">当前状态</h3>
        <div class="device-detail__state">
          <span>设备状态</span>
          <StatusChip
            :tone="deviceStateVisual(detail.state).tone"
            :label="deviceStateVisual(detail.state).label"
          />
        </div>
        <div class="device-detail__state">
          <span>Gateway</span>
          <StatusChip
            :tone="gatewayStateVisual(detail.gatewayState).tone"
            :label="gatewayStateVisual(detail.gatewayState).label"
          />
        </div>
      </section>
      <section aria-labelledby="device-detail-meta">
        <h3 id="device-detail-meta" class="device-detail__heading">标识与凭证</h3>
        <dl class="device-detail__list">
          <div>
            <dt>设备 ID（前 8 位）</dt>
            <dd class="mono break-anywhere">{{ shortDeviceId(detail.id) }}</dd>
          </div>
          <div>
            <dt>完整 Device UUID</dt>
            <dd class="device-detail__uuid">
              <CopyValue :value="detail.fullDeviceUuid" aria-label-copy="复制完整 Device UUID" />
            </dd>
          </div>
          <div>
            <dt>Token 格式版本</dt>
            <dd class="mono break-anywhere">pdv{{ detail.tokenFormatVersion }}</dd>
          </div>
          <div>
            <dt>客户端版本</dt>
            <dd>{{ detail.clientVersion ?? '未知' }}</dd>
          </div>
          <div>
            <dt>Scopes</dt>
            <dd>{{ detail.scopes.join(' · ') }}</dd>
          </div>
          <div>
            <dt>Capabilities</dt>
            <dd>{{ detail.capabilities.join(' · ') }}</dd>
          </div>
        </dl>
      </section>
      <section aria-labelledby="device-detail-times">
        <h3 id="device-detail-times" class="device-detail__heading">时间</h3>
        <dl class="device-detail__list">
          <div>
            <dt>创建时间</dt>
            <dd class="tabular">{{ new Date(detail.createdAt).toISOString() }}</dd>
          </div>
          <div>
            <dt>更新时间</dt>
            <dd class="tabular">{{ new Date(detail.updatedAt).toISOString() }}</dd>
          </div>
          <div>
            <dt>最后在线</dt>
            <dd><TimeAgo :timestamp="detail.lastSeenAt" /></dd>
          </div>
          <div>
            <dt>最后心跳</dt>
            <dd><TimeAgo :timestamp="detail.lastHeartbeatAt" /></dd>
          </div>
          <div>
            <dt>上次 Rotate</dt>
            <dd><TimeAgo :timestamp="detail.lastRotatedAt" /></dd>
          </div>
          <div>
            <dt>Gateway 连接时间</dt>
            <dd><TimeAgo :timestamp="detail.connectedAt" /></dd>
          </div>
          <div>
            <dt>Gateway Generation</dt>
            <dd class="mono break-anywhere">{{ detail.gatewayGeneration ?? '—' }}</dd>
          </div>
        </dl>
      </section>
    </template>
    <EmptyState v-else title="未选择设备" description="在设备列表中选择详情操作即可查看。" />
  </AppDrawer>
</template>

<style scoped>
.device-detail__heading {
  margin-bottom: var(--pd-space-8);
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-13);
  font-weight: var(--pd-font-weight-semibold);
  line-height: var(--pd-line-height-ui);
}

.device-detail__state {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: var(--pd-space-8) 0;
  border-bottom: 1px solid var(--pd-border-separator);
  font-size: var(--pd-font-size-13);
  line-height: var(--pd-line-height-ui);
}

.device-detail__list {
  display: flex;
  flex-direction: column;
  gap: var(--pd-space-8);
}

.device-detail__list div {
  display: flex;
  justify-content: space-between;
  gap: var(--pd-space-12);
  min-width: 0;
  font-size: var(--pd-font-size-13);
  line-height: var(--pd-line-height-ui);
}

.device-detail__list dt {
  flex-shrink: 0;
  color: var(--pd-text-muted);
}

.device-detail__list dd {
  min-width: 0;
  color: var(--pd-text-default);
  font-weight: var(--pd-font-weight-medium);
  text-align: end;
  overflow-wrap: anywhere;
}

.device-detail__uuid {
  font-family: var(--pd-font-code);
  font-size: var(--pd-font-size-11);
}

.device-detail__uuid :deep(.copy-value__text) {
  overflow: visible;
  overflow-wrap: anywhere;
  text-overflow: clip;
  white-space: normal;
}
</style>
