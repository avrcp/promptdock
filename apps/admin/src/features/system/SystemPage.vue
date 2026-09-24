<script setup lang="ts">
import { computed, ref } from 'vue'
import {
  RefreshCw,
  HardDrive,
  Cpu,
  Database,
  Hammer,
  SlidersHorizontal,
  AlertTriangle,
  ShieldCheck,
} from 'lucide-vue-next'

import StatusChip from '@/components/StatusChip.vue'
import AppButton from '@/components/AppButton.vue'
import AppDialog from '@/components/AppDialog.vue'
import AdminErrorAlert from '@/components/AdminErrorAlert.vue'
import InlineAlert from '@/components/InlineAlert.vue'
import SkeletonBlock from '@/components/SkeletonBlock.vue'
import EmptyState from '@/components/EmptyState.vue'
import TimeAgo from '@/components/TimeAgo.vue'
import CopyValue from '@/components/CopyValue.vue'
import {
  workerStateVisual,
  databaseIntegrityVisual,
  databaseSizeBucketVisual,
  retentionResultVisual,
  bindClassVisual,
  poolHealthVisual,
} from './system-status'
import { useAsyncResource } from '@/composables/useAsyncResource'
import { useAdminRepository } from '@/composables/useAdminRepository'
import { useAdminCapabilities } from '@/composables/useAdminCapabilities'
import { useNow } from '@/composables/useNow'
import { runtimeEnvironment } from '@/app/environment'
import { toAdminError, type AdminError } from '@/contracts/error'
import type { SystemSnapshot } from '@/contracts/system'
import type { MaintenanceReceipt } from '@/contracts/common'

const adminRepository = useAdminRepository()
const { can: canCapability, capabilityReason } = useAdminCapabilities()
const { now } = useNow()

const WORKER_STALE_THRESHOLD_MS = 60_000

function workerHeartbeatStale(timestamp: number | null): boolean {
  return timestamp === null || now.value - timestamp > WORKER_STALE_THRESHOLD_MS
}

const systemResource = useAsyncResource<SystemSnapshot>({
  fetcher: (signal) => adminRepository.read.getSystemSnapshot({ signal }),
})

const loading = computed(() => systemResource.loading.value && systemResource.data.value === null)
const refreshing = computed(() => systemResource.refreshing.value)
const error = computed<AdminError | null>(() => systemResource.error.value)
const snapshot = computed<SystemSnapshot | null>(() => systemResource.data.value)

async function refresh(): Promise<void> {
  await systemResource.refresh('refresh')
}

const retentionInFlight = ref(false)
const retentionError = ref<AdminError | null>(null)
const retentionReceipt = ref<MaintenanceReceipt | null>(null)
const retentionDialogOpen = ref(false)

function openRetentionConfirmation(): void {
  if (!canCapability('admin_maintenance_v2')) return
  retentionError.value = null
  retentionDialogOpen.value = true
}

async function runRetention(): Promise<void> {
  if (!canCapability('admin_maintenance_v2')) return
  if (retentionInFlight.value) return
  retentionInFlight.value = true
  retentionError.value = null
  try {
    retentionReceipt.value = await adminRepository.command.runRetention()
    retentionDialogOpen.value = false
    await refresh()
  } catch (err) {
    retentionError.value = toAdminError(err, {
      code: 'INTERNAL',
      message: '保留清理失败，请重试。',
      retryable: true,
      requestId: null,
    })
  } finally {
    retentionInFlight.value = false
  }
}

const diagnosticsOpen = ref(false)
const diagnosticsPrefix = computed<string>(() => {
  // Production builds must not be mislabelled as 模拟 (mock).  Mock builds
  // surface the prefix so the operator can never confuse a synthetic
  // snapshot for live Relay state.
  return runtimeEnvironment.mode === 'production' ? '诊断摘要：' : '诊断摘要（模拟数据）：'
})
const diagnosticsSummary = computed<string>(() => {
  const s = snapshot.value
  if (!s) return ''
  const alertCount = s.currentAlerts.length
  const runningWorkers = s.workers.filter((w) => w.state === 'running').length
  return `${diagnosticsPrefix.value}构建 ${s.build.relayVersion}；数据库完整性 ${databaseIntegrityVisual(s.database.integrityStatus).label}；${runningWorkers}/${s.workers.length} 个 worker 运行中；投递保留 ${s.retention.acceptedDays} 天；检测到 ${alertCount} 个当前问题。未包含任何密钥或 token。`
})

function openDiagnostics(): void {
  diagnosticsOpen.value = true
}
</script>

<template>
  <section class="system">
    <AdminErrorAlert v-if="error" :error="error" testid="system-error" @retry="refresh" />

    <div
      v-if="loading"
      class="system__skeleton"
      data-testid="system-skeleton"
      role="status"
      aria-live="polite"
    >
      <span class="sr-only">正在加载系统快照</span>
      <SkeletonBlock v-for="i in 5" :key="i" variant="panel" />
    </div>

    <template v-else-if="snapshot">
      <div class="system__ops" role="region" aria-label="运维动作">
        <AppButton
          variant="secondary"
          size="sm"
          :loading="refreshing"
          data-testid="system-refresh"
          @click="refresh"
        >
          <template #default>
            <RefreshCw :size="14" aria-hidden="true" />
            <span>刷新诊断</span>
          </template>
        </AppButton>
        <AppButton
          variant="secondary"
          size="sm"
          data-testid="system-generate-diagnostics"
          @click="openDiagnostics"
        >
          <template #default>
            <ShieldCheck :size="14" aria-hidden="true" />
            <span>生成安全诊断摘要</span>
          </template>
        </AppButton>
        <AppButton
          class="system__ops-maintenance"
          variant="danger"
          size="sm"
          :loading="retentionInFlight"
          :disabled="!canCapability('admin_maintenance_v2')"
          :aria-describedby="
            capabilityReason('admin_maintenance_v2') ? 'maintenance-capability-reason' : undefined
          "
          data-testid="system-run-retention"
          @click="openRetentionConfirmation"
        >
          <template #default>
            <Hammer :size="14" aria-hidden="true" />
            <span>执行保留清理</span>
          </template>
        </AppButton>
        <p
          v-if="capabilityReason('admin_maintenance_v2')"
          id="maintenance-capability-reason"
          class="system__capability-hint"
          data-testid="maintenance-capability-reason"
        >
          执行保留清理：{{ capabilityReason('admin_maintenance_v2') }}
        </p>
      </div>

      <AdminErrorAlert v-if="retentionError" :error="retentionError" @retry="runRetention" />
      <InlineAlert
        v-else-if="retentionReceipt"
        tone="success"
        title="保留清理已完成"
        :description="retentionReceipt.summary"
      />

      <section
        v-if="diagnosticsOpen"
        class="system__diagnostics"
        role="status"
        aria-labelledby="system-diagnostics-title"
        data-testid="system-diagnostics-alert"
      >
        <header class="system__diagnostics-head">
          <div>
            <h2 id="system-diagnostics-title">安全诊断摘要</h2>
            <p>仅包含当前系统状态，不含密钥或 token。</p>
          </div>
        </header>
        <CopyValue
          class="system__diagnostics-copy"
          :value="diagnosticsSummary"
          aria-label-copy="复制安全诊断摘要"
        />
      </section>

      <AppDialog
        v-model:open="retentionDialogOpen"
        title="确认执行保留清理"
        description="Relay 将按当前服务端保留策略永久删除已到期的终态记录。此操作不可撤销。"
        primary-label="确认并执行清理"
        primary-variant="danger"
        :primary-loading="retentionInFlight"
        :primary-disabled="!canCapability('admin_maintenance_v2')"
        :close-on-backdrop="!retentionInFlight"
        :close-on-escape="!retentionInFlight"
        @primary="runRetention"
      >
        <dl class="system__retention-preview" data-testid="retention-scope-preview">
          <div>
            <dt>已接收投递</dt>
            <dd>早于 {{ snapshot.retention.acceptedDays }} 天</dd>
          </div>
          <div>
            <dt>死信投递</dt>
            <dd>早于 {{ snapshot.retention.deadLetterDays }} 天</dd>
          </div>
          <div>
            <dt>终态入站命令</dt>
            <dd>早于 {{ snapshot.retention.inboundTerminalDays }} 天</dd>
          </div>
          <div>
            <dt>过期入站命令</dt>
            <dd>早于 {{ snapshot.retention.inboundExpiredDays }} 天</dd>
          </div>
        </dl>
        <InlineAlert
          v-if="retentionError"
          tone="danger"
          :title="retentionError.message"
          :code="retentionError.code"
        />
      </AppDialog>

      <div class="system__grid">
        <!-- Build -->
        <section class="system__card system__card--build" aria-labelledby="system-build-title">
          <h2 id="system-build-title" class="system__card-title">
            <Cpu :size="16" aria-hidden="true" class="system__card-icon" />
            构建
          </h2>
          <dl class="system__meta system__meta--compact">
            <div>
              <dt>Relay 版本</dt>
              <dd class="system__mono">{{ snapshot.build.relayVersion }}</dd>
            </div>
            <div>
              <dt>Relay Commit</dt>
              <dd class="system__mono">{{ snapshot.build.gitCommit }}</dd>
            </div>
            <div>
              <dt>构建时间</dt>
              <dd class="system__mono">{{ snapshot.build.buildTime }}</dd>
            </div>
            <div>
              <dt>Rust 版本</dt>
              <dd class="system__mono">{{ snapshot.build.rustVersion }}</dd>
            </div>
            <div>
              <dt>API 版本</dt>
              <dd class="system__mono">{{ snapshot.build.apiVersion }}</dd>
            </div>
            <div>
              <dt>Gateway 版本</dt>
              <dd class="system__mono">{{ snapshot.build.gatewayVersion }}</dd>
            </div>
            <div>
              <dt>Release 版本</dt>
              <dd class="system__mono">{{ runtimeEnvironment.buildVersion }}</dd>
            </div>
            <div>
              <dt>Source Commit</dt>
              <dd class="system__mono">{{ runtimeEnvironment.buildCommit.slice(0, 12) }}</dd>
            </div>
            <div>
              <dt>Admin API Major</dt>
              <dd class="system__mono">v{{ runtimeEnvironment.adminApiMajor }}</dd>
            </div>
          </dl>
        </section>

        <!-- Database -->
        <section
          class="system__card system__card--database"
          :class="{
            'system__card--attention': snapshot.database.integrityStatus !== 'ok',
          }"
          aria-labelledby="system-db-title"
          data-testid="system-database-panel"
        >
          <h2 id="system-db-title" class="system__card-title">
            <Database :size="16" aria-hidden="true" class="system__card-icon" />
            数据库
            <StatusChip
              :tone="databaseIntegrityVisual(snapshot.database.integrityStatus).tone"
              :label="databaseIntegrityVisual(snapshot.database.integrityStatus).label"
            />
          </h2>
          <dl class="system__meta system__meta--compact">
            <div>
              <dt>Schema 标识</dt>
              <dd class="system__mono">{{ snapshot.database.schemaIdentity }}</dd>
            </div>
            <div>
              <dt>Schema 版本</dt>
              <dd class="mono break-anywhere">{{ snapshot.database.schemaRevision }}</dd>
            </div>
            <div>
              <dt>WAL</dt>
              <dd>
                {{
                  snapshot.database.walStatus === 'enabled'
                    ? '已启用'
                    : snapshot.database.walStatus === 'disabled'
                      ? '已禁用'
                      : '未知'
                }}
              </dd>
            </div>
            <div>
              <dt>外键约束</dt>
              <dd>{{ snapshot.database.foreignKeysEnabled ? '已启用' : '已禁用' }}</dd>
            </div>
            <div>
              <dt>连接池</dt>
              <dd>
                <StatusChip
                  :tone="poolHealthVisual(snapshot.database.poolHealth).tone"
                  :label="poolHealthVisual(snapshot.database.poolHealth).label"
                />
              </dd>
            </div>
            <div>
              <dt>规模</dt>
              <dd>{{ databaseSizeBucketVisual(snapshot.database.sizeBucket) }}</dd>
            </div>
            <div>
              <dt>上次保留</dt>
              <dd><TimeAgo :timestamp="snapshot.database.lastRetentionPassAt" /></dd>
            </div>
          </dl>
        </section>

        <!-- Retention -->
        <section
          class="system__card system__card--retention"
          aria-labelledby="system-retention-title"
        >
          <h2 id="system-retention-title" class="system__card-title">
            <Hammer :size="16" aria-hidden="true" class="system__card-icon" />
            保留策略
          </h2>
          <dl class="system__meta system__meta--compact">
            <div>
              <dt>已接受投递</dt>
              <dd class="tabular">{{ snapshot.retention.acceptedDays }} 天</dd>
            </div>
            <div>
              <dt>死信</dt>
              <dd class="tabular">{{ snapshot.retention.deadLetterDays }} 天</dd>
            </div>
            <div>
              <dt>入站终态</dt>
              <dd class="tabular">{{ snapshot.retention.inboundTerminalDays }} 天</dd>
            </div>
            <div>
              <dt>入站过期</dt>
              <dd class="tabular">{{ snapshot.retention.inboundExpiredDays }} 天</dd>
            </div>
            <div>
              <dt>上次运行</dt>
              <dd><TimeAgo :timestamp="snapshot.retention.lastRunAt" /></dd>
            </div>
            <div>
              <dt>上次结果</dt>
              <dd>
                <StatusChip
                  :tone="retentionResultVisual(snapshot.retention.lastResult).tone"
                  :label="retentionResultVisual(snapshot.retention.lastResult).label"
                />
              </dd>
            </div>
          </dl>
        </section>

        <!-- Workers -->
        <section class="system__card system__card--workers" aria-labelledby="system-workers-title">
          <h2 id="system-workers-title" class="system__card-title">
            <Cpu :size="16" aria-hidden="true" class="system__card-icon" />
            Workers
          </h2>
          <ul class="system__workers">
            <li v-for="w in snapshot.workers" :key="w.name" class="system__worker">
              <div class="system__worker-line system__worker-line--primary">
                <span class="system__worker-name" :title="w.name">{{ w.name }}</span>
                <StatusChip
                  :tone="workerStateVisual(w.state).tone"
                  :label="workerStateVisual(w.state).label"
                />
              </div>
              <div class="system__worker-line system__worker-line--secondary">
                <span
                  class="system__worker-heartbeat tabular"
                  :class="{ 'system__worker-heartbeat--stale': workerHeartbeatStale(w.lastTickAt) }"
                >
                  心跳
                  <TimeAgo :timestamp="w.lastTickAt" />
                </span>
                <span class="system__worker-detail" :title="w.detail ?? '—'">
                  {{ w.detail ?? '—' }}
                </span>
              </div>
            </li>
          </ul>
        </section>

        <!-- Configuration summary -->
        <section class="system__card system__card--config" aria-labelledby="system-config-title">
          <h2 id="system-config-title" class="system__card-title">
            <SlidersHorizontal :size="16" aria-hidden="true" class="system__card-icon" />
            配置摘要
          </h2>
          <dl class="system__meta">
            <div>
              <dt>微信通道</dt>
              <dd>
                <StatusChip
                  :tone="snapshot.configuration.wechatEnabled ? 'success' : 'muted'"
                  :label="snapshot.configuration.wechatEnabled ? '已启用' : '未启用'"
                />
              </dd>
            </div>
            <div>
              <dt>公开监听</dt>
              <dd>{{ bindClassVisual(snapshot.configuration.publicBindClass) }}</dd>
            </div>
            <div>
              <dt>管理监听</dt>
              <dd>{{ bindClassVisual(snapshot.configuration.adminBindClass) }}</dd>
            </div>
            <div>
              <dt>管理模式</dt>
              <dd>{{ snapshot.configuration.adminMode === 'operator' ? '运维' : '只读' }}</dd>
            </div>
            <div>
              <dt>已观测上游 HTTPS</dt>
              <dd>{{ snapshot.configuration.forwardedHttpsObserved ? '是' : '否' }}</dd>
            </div>
          </dl>
          <div class="system__flags">
            <span class="system__flags-label">特性开关</span>
            <div class="system__flags-list">
              <span
                v-if="snapshot.configuration.featureFlags.length === 0"
                class="system__flag system__flag--empty"
              >
                无
              </span>
              <span v-for="f in snapshot.configuration.featureFlags" :key="f" class="system__flag">
                {{ f }}
              </span>
            </div>
          </div>
        </section>

        <!-- Issues -->
        <section
          v-if="snapshot.currentAlerts.length > 0"
          class="system__card system__card--issues"
          aria-labelledby="system-alerts-title"
        >
          <h2 id="system-alerts-title" class="system__card-title">
            <AlertTriangle :size="16" aria-hidden="true" class="system__card-icon" />
            当前问题
          </h2>
          <ul class="system__issues">
            <li
              v-for="issue in snapshot.currentAlerts"
              :key="issue.id"
              class="system__issue"
              :class="`system__issue--${issue.severity}`"
            >
              <div class="system__issue-head">
                <span class="system__issue-msg">{{ issue.message }}</span>
                <StatusChip
                  class="mono"
                  :tone="
                    issue.severity === 'error'
                      ? 'danger'
                      : issue.severity === 'warning'
                        ? 'warning'
                        : 'info'
                  "
                  :label="issue.code"
                />
              </div>
              <div class="system__issue-meta">
                <span>组件 {{ issue.component }}</span>
                <span>
                  ·
                  <TimeAgo :timestamp="issue.observedAt" />
                </span>
              </div>
            </li>
          </ul>
        </section>
      </div>
    </template>

    <EmptyState
      v-else-if="!error"
      :icon="HardDrive"
      title="没有系统数据"
      description="当前 scenario 未提供系统快照。"
    />
  </section>
</template>

<style scoped>
.system {
  min-width: 0;
  container-type: inline-size;
}

.system__ops {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--pd-space-8);
  margin-bottom: var(--pd-space-12);
  padding: var(--pd-space-8);
  background: var(--pd-container-content-bg);
  border: 1px solid var(--pd-border-separator);
  border-radius: var(--pd-radius-md);
}

.system__capability-hint {
  flex-basis: 100%;
  color: var(--pd-text-subtle);
  font-size: var(--pd-font-size-12);
  line-height: var(--pd-line-height-metadata);
}

.system__ops-maintenance {
  margin-inline-start: auto;
}

.system > :deep(.admin-error-alert),
.system > :deep(.inline-alert) {
  margin-bottom: var(--pd-space-12);
}

.system__grid {
  display: grid;
  grid-template-columns: minmax(0, 1fr);
  align-items: start;
  gap: var(--pd-space-12);
}

.system__skeleton {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(min(100%, 22rem), 1fr));
  align-items: start;
  gap: var(--pd-space-12);
}

.system__card {
  min-width: 0;
  overflow: hidden;
  background: var(--pd-container-panel-bg);
  border: 1px solid var(--pd-border-separator);
  border-radius: var(--pd-radius-md);
}

.system__card--config {
  container-type: inline-size;
}

.system__card--attention {
  border-color: var(--pd-feedback-warning-muted);
}

.system__card-title {
  display: flex;
  align-items: center;
  gap: var(--pd-space-8);
  min-height: var(--pd-panel-header-height);
  padding: 0 var(--pd-space-12);
  border-bottom: 1px solid var(--pd-border-separator);
  color: var(--pd-text-default);
  font-size: var(--pd-font-size-13);
  font-weight: var(--pd-font-weight-semibold);
}

.system__card-title :deep(.status-chip) {
  margin-inline-start: auto;
}

.system__card-icon {
  flex: 0 0 auto;
  color: var(--pd-text-subtle);
}

.system__card--attention .system__card-icon {
  color: var(--pd-feedback-warning);
}

.system__meta {
  margin: 0;
}

.system__meta > div {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--pd-space-16);
  padding: var(--pd-space-4) var(--pd-space-12);
  border-bottom: 1px solid var(--pd-border-separator);
}

.system__meta > div:last-child {
  border-bottom: 0;
}

.system__meta dt {
  flex: 0 0 auto;
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-12);
}

.system__meta dd {
  min-width: 0;
  margin: 0;
  overflow-wrap: anywhere;
  color: var(--pd-text-default);
  font-size: var(--pd-font-size-13);
  font-weight: var(--pd-font-weight-medium);
  text-align: end;
}

.system__meta--compact > div {
  display: grid;
  grid-template-columns: minmax(0, 1fr) minmax(0, auto);
}

.system__mono {
  font-family: var(--pd-font-code);
  font-size: var(--pd-font-size-12);
  font-weight: var(--pd-font-weight-regular);
}

.system__card--config .system__meta {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(min(100%, 16rem), 1fr));
}

.system__card--config .system__meta > div:last-child:nth-child(odd) {
  grid-column: 1 / -1;
}

.system__workers,
.system__issues {
  display: grid;
}

.system__worker {
  display: grid;
  gap: var(--pd-space-4);
  min-height: var(--pd-table-row-height);
  padding: var(--pd-space-8) var(--pd-space-12);
  border-bottom: 1px solid var(--pd-border-separator);
}

.system__worker:last-child,
.system__issue:last-child {
  border-bottom: 0;
}

.system__worker-line {
  display: flex;
  min-width: 0;
  align-items: center;
  justify-content: space-between;
  gap: var(--pd-space-12);
}

.system__worker-name {
  min-width: 0;
  overflow: hidden;
  color: var(--pd-text-default);
  font-family: var(--pd-font-code);
  font-size: var(--pd-font-size-12);
  font-weight: var(--pd-font-weight-medium);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.system__worker-heartbeat {
  color: var(--pd-text-subtle);
  font-size: var(--pd-font-size-11);
  white-space: nowrap;
}

.system__worker-heartbeat--stale {
  color: var(--pd-feedback-warning);
}

.system__worker-detail {
  min-width: 0;
  flex: 1 1 auto;
  overflow: hidden;
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-12);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.system__flags {
  display: grid;
  gap: var(--pd-space-8);
  padding: var(--pd-space-12);
  border-top: 1px solid var(--pd-border-separator);
}

.system__flags-label {
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-12);
}

.system__flags-list {
  display: flex;
  flex-wrap: wrap;
  gap: var(--pd-space-4);
}

.system__flag {
  display: inline-flex;
  align-items: center;
  min-height: var(--pd-status-chip-height);
  padding: 0 var(--pd-space-8);
  background: var(--pd-container-content-bg);
  border: 1px solid var(--pd-border-separator);
  border-radius: var(--pd-radius-sm);
  color: var(--pd-text-muted);
  font-family: var(--pd-font-code);
  font-size: var(--pd-font-size-11);
}

.system__flag--empty {
  color: var(--pd-text-subtle);
}

.system__issue {
  padding: var(--pd-space-8) var(--pd-space-12) var(--pd-space-8) var(--pd-space-8);
  border-bottom: 1px solid var(--pd-border-separator);
  border-inline-start: 2px solid var(--pd-feedback-info);
}

.system__issue--warning {
  border-inline-start-color: var(--pd-feedback-warning);
}

.system__issue--error {
  border-inline-start-color: var(--pd-feedback-danger);
}

.system__issue-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--pd-space-12);
}

.system__issue-msg {
  min-width: 0;
  color: var(--pd-text-default);
  font-size: var(--pd-font-size-13);
  font-weight: var(--pd-font-weight-medium);
}

.system__issue-meta {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--pd-space-8);
  margin-top: var(--pd-space-4);
  color: var(--pd-text-subtle);
  font-family: var(--pd-font-code);
  font-size: var(--pd-font-size-11);
}

.system__skeleton {
  margin-bottom: var(--pd-space-12);
}

.system__retention-preview {
  display: grid;
  margin: 0;
  border-top: 1px solid var(--pd-border-separator);
}

.system__retention-preview div {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--pd-space-16);
  min-height: var(--pd-control-height-md);
  border-bottom: 1px solid var(--pd-border-separator);
  font-size: var(--pd-font-size-13);
}

.system__retention-preview dt {
  color: var(--pd-text-muted);
}

.system__retention-preview dd {
  margin: 0;
  color: var(--pd-text-default);
  font-family: var(--pd-font-code);
  font-size: var(--pd-font-size-12);
  font-variant-numeric: tabular-nums;
}

.system__diagnostics {
  margin-bottom: var(--pd-space-12);
  overflow: hidden;
  background: var(--pd-container-sunken-bg);
  border: 1px solid var(--pd-border-separator);
  border-radius: var(--pd-radius-md);
}

.system__diagnostics-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--pd-space-12);
  padding: var(--pd-space-8) var(--pd-space-12);
  border-bottom: 1px solid var(--pd-border-separator);
}

.system__diagnostics-head h2 {
  color: var(--pd-text-default);
  font-size: var(--pd-font-size-13);
  font-weight: var(--pd-font-weight-semibold);
}

.system__diagnostics-head p {
  margin-top: var(--pd-space-2);
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-11);
}

.system__diagnostics-copy {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  align-items: start;
  gap: var(--pd-space-12);
  padding: var(--pd-space-12);
}

.system__diagnostics-copy :deep(.copy-value__text) {
  max-width: none;
  overflow: visible;
  color: var(--pd-text-default);
  font-size: var(--pd-font-size-12);
  line-height: var(--pd-line-height-reading);
  text-overflow: clip;
  white-space: normal;
}

@container (min-width: 44rem) {
  .system__grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .system__card--issues {
    grid-column: 1 / -1;
  }
}

@container (min-width: 60rem) {
  .system__grid {
    grid-template-columns: repeat(12, minmax(0, 1fr));
  }

  .system__card--build,
  .system__card--database,
  .system__card--retention {
    grid-column: span 4;
  }

  .system__card--workers {
    grid-column: span 5;
  }

  .system__card--config {
    grid-column: span 7;
  }

  .system__card--issues {
    grid-column: span 12;
  }
}

@container (max-width: 43.999rem) {
  .system__ops-maintenance {
    margin-inline-start: 0;
  }

  .system__worker-line--secondary {
    align-items: flex-start;
  }

  .system__worker-detail {
    overflow: visible;
    overflow-wrap: anywhere;
    text-overflow: clip;
    white-space: normal;
  }
}
</style>
