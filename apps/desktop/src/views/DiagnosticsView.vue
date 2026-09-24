<script setup lang="ts">
import { computed, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import ConfirmDialog from '../components/ConfirmDialog.vue'
import PanelSection from '../components/PanelSection.vue'
import HookHealthPanel from '../components/HookHealthPanel.vue'
import StatusChip from '../components/StatusChip.vue'
import { useDesktopState } from '../desktop/desktopState'

const { status, autostart, busy, diagnostics, act, showDiagnostics, navigate } = useDesktopState()
type StepStatus = 'pass' | 'attention' | 'not_run' | 'unavailable'
type NextAction =
  'open_integration' | 'probe_relay' | 'open_notifications' | 'view_deliveries' | 'none'
type Step = {
  id: string
  label: string
  status: StepStatus
  code: string | null
  detail: string
  nextAction: NextAction
}
type TestProbe = {
  probeId: string
  outboxId: string
  status: string
  remoteStatus: string | null
  lastErrorCode: string | null
}
const health = ref<{
  steps: Step[]
  report: Record<string, unknown>
  test: TestProbe | null
} | null>(null)
const healthBusy = ref(false)
const healthError = ref('')
const exportOpen = ref(false)
const testBusy = ref(false)
const test = ref<TestProbe | null>(null)
let testRequestId: string | null = null
function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}
function isTest(value: unknown): value is TestProbe {
  return (
    isRecord(value) &&
    typeof value.probeId === 'string' &&
    value.probeId.length > 0 &&
    typeof value.outboxId === 'string' &&
    value.outboxId.length > 0 &&
    typeof value.status === 'string' &&
    (value.remoteStatus === null || typeof value.remoteStatus === 'string') &&
    (value.lastErrorCode === null || typeof value.lastErrorCode === 'string')
  )
}
function isStep(value: unknown): value is Step {
  return (
    isRecord(value) &&
    typeof value.id === 'string' &&
    value.id.length > 0 &&
    typeof value.label === 'string' &&
    value.label.length > 0 &&
    ['pass', 'attention', 'not_run', 'unavailable'].includes(value.status as string) &&
    (value.code === null || typeof value.code === 'string') &&
    typeof value.detail === 'string' &&
    value.detail.length > 0 &&
    ['open_integration', 'probe_relay', 'open_notifications', 'view_deliveries', 'none'].includes(
      value.nextAction as string,
    )
  )
}
function isHealth(value: unknown): value is NonNullable<typeof health.value> {
  return (
    isRecord(value) &&
    Array.isArray(value.steps) &&
    value.steps.every(isStep) &&
    isRecord(value.report) &&
    (value.test === null || isTest(value.test))
  )
}
const statusLabel: Record<StepStatus, string> = {
  pass: '通过',
  attention: '需要处理',
  not_run: '未执行',
  unavailable: '不可用',
}
const actionLabel: Record<Exclude<NextAction, 'none'>, string> = {
  open_integration: '查看 Hook 配置',
  probe_relay: '检查服务器',
  open_notifications: '查看通知设置',
  view_deliveries: '查看投递',
}
const displayedTest = computed(() => test.value ?? health.value?.test ?? null)
async function checkHealth() {
  if (healthBusy.value) return
  healthBusy.value = true
  healthError.value = ''
  try {
    const next = await invoke<unknown>('desktop_health_check')
    if (!isHealth(next)) throw new Error('体检回执无法确认。')
    health.value = next
    test.value = next.test
  } catch (error) {
    healthError.value = error instanceof Error ? error.message : '本机体检未完成。'
  } finally {
    healthBusy.value = false
  }
}
async function exportHealth() {
  exportOpen.value = false
  healthBusy.value = true
  try {
    const receipt = await invoke<unknown>('desktop_health_export')
    if (!isRecord(receipt) || typeof receipt.path !== 'string' || !receipt.path)
      throw new Error('导出回执无法确认。')
    healthError.value = `已导出脱敏体检：${receipt.path}`
  } catch (error) {
    healthError.value = error instanceof Error ? error.message : '导出未完成。'
  } finally {
    healthBusy.value = false
  }
}
async function checkTest(allowWhileBusy = false) {
  if (testBusy.value && !allowWhileBusy) return
  testBusy.value = true
  try {
    const receipt = await invoke<unknown>('desktop_health_test_status')
    if (receipt !== null && !isTest(receipt)) throw new Error('测试状态回执无法确认。')
    test.value = receipt
  } catch (error) {
    healthError.value = error instanceof Error ? error.message : '测试状态暂不可读取。'
  } finally {
    testBusy.value = false
  }
}
async function startTest() {
  if (testBusy.value) return
  testBusy.value = true
  healthError.value = ''
  testRequestId ??= crypto.randomUUID()
  try {
    const receipt = await invoke<unknown>('desktop_health_test_start', { requestId: testRequestId })
    if (!isTest(receipt)) throw new Error('测试通知回执无法确认。')
    test.value = receipt
    await checkHealth()
  } catch (error) {
    healthError.value =
      error instanceof Error ? error.message : '测试通知未确认，正在查询已提交状态。'
    await checkTest(true)
  } finally {
    testBusy.value = false
  }
}
function newTest() {
  testRequestId = null
  test.value = null
  void startTest()
}
function next(step: Step) {
  if (step.nextAction === 'open_integration') navigate('integration')
  else if (step.nextAction === 'open_notifications') navigate('notifications')
  else if (step.nextAction === 'view_deliveries') navigate('deliveries')
  else if (step.nextAction === 'probe_relay') act('desktop_relay_probe')
}
</script>

<template>
  <div class="view form-view">
    <PanelSection
      title="本机体检"
      description="体检只读取本机安全诊断；不会自动访问服务器、发送测试通知或安装 Hook。"
    >
      <div class="panel__actions">
        <button
          class="button button--primary"
          type="button"
          :disabled="healthBusy"
          @click="checkHealth"
        >
          {{ healthBusy ? '正在体检…' : '运行本机体检' }}
        </button>
        <button
          class="button button--secondary"
          type="button"
          :disabled="!health || healthBusy"
          @click="exportOpen = true"
        >
          导出脱敏体检
        </button>
        <button
          class="button button--secondary"
          type="button"
          :disabled="testBusy"
          @click="testRequestId ? checkTest() : startTest()"
        >
          {{ testRequestId ? '查看测试状态' : '发送测试通知' }}
        </button>
        <button
          v-if="testRequestId"
          class="button button--ghost"
          type="button"
          :disabled="testBusy"
          @click="newTest"
        >
          发送新的测试通知
        </button>
      </div>
      <p class="field__hint">测试通知会提交给 Relay；服务器接管不代表手机已显示。</p>
      <p v-if="healthError" class="field-error" role="status">{{ healthError }}</p>
      <ul v-if="health" class="health-steps">
        <li v-for="step in health.steps" :key="step.id">
          <StatusChip
            :tone="
              step.status === 'pass'
                ? 'success'
                : step.status === 'attention'
                  ? 'warning'
                  : 'neutral'
            "
            :label="`${step.label}：${statusLabel[step.status]}`"
          />
          <span>{{ step.detail }}</span
          ><code v-if="step.code">{{ step.code }}</code>
          <button
            v-if="step.nextAction !== 'none'"
            class="button button--ghost"
            type="button"
            @click="next(step)"
          >
            {{ actionLabel[step.nextAction] }}
          </button>
        </li>
      </ul>
      <p v-if="displayedTest" class="field__hint">
        测试 {{ displayedTest.status }}（{{ displayedTest.probeId.slice(0, 8) }} /
        {{ displayedTest.outboxId.slice(0, 8) }}）；Relay：{{
          displayedTest.remoteStatus ?? '尚未确认'
        }}。手机显示仍为 NOT_RUN，需人工核验。
      </p>
    </PanelSection>

    <HookHealthPanel />
    <PanelSection
      title="后台运行"
      description="关闭窗口后应用保留在托盘中处理通知；从托盘菜单可完全退出。"
    >
      <template #actions>
        <StatusChip tone="neutral" label="应用已启动" />
      </template>

      <div class="stack panel__body">
        <label class="toggle-row">
          <input
            id="autostart"
            :checked="autostart"
            type="checkbox"
            :disabled="busy"
            aria-describedby="autostart-hint"
            @change="act('desktop_autostart_set', { enabled: !autostart })"
          />
          <span class="toggle-row__text">
            <strong class="toggle-row__title">开机自启</strong>
            <small id="autostart-hint" class="toggle-row__hint"
              >登录 Windows 后在后台启动 PromptDock。</small
            >
          </span>
        </label>
        <dl class="summary-list summary-list--single">
          <div>
            <dt>受保护数据目录</dt>
            <dd class="mono">{{ status?.dataDirectory }}</dd>
          </div>
        </dl>
      </div>
    </PanelSection>

    <PanelSection
      title="脱敏诊断"
      description="仅显示运行状态和错误码，不包含提示词、回答正文、工作目录或凭据。"
    >
      <div class="stack panel__body">
        <button
          class="button button--secondary diagnostics-button"
          type="button"
          :disabled="busy"
          @click="showDiagnostics"
        >
          查看脱敏诊断
        </button>
        <pre v-if="diagnostics" class="diagnostics-output mono" aria-label="脱敏诊断">{{
          diagnostics
        }}</pre>
      </div>
    </PanelSection>

    <ConfirmDialog
      :open="exportOpen"
      title="导出脱敏体检？"
      description="导出包含构建身份、计数、安全错误码和最近诊断；不包含正文或凭据。"
      confirm-label="导出体检"
      :busy="healthBusy"
      @cancel="exportOpen = false"
      @confirm="exportHealth"
    />
  </div>
</template>

<style scoped>
.form-view {
  display: grid;
  gap: var(--space-3);
}

.diagnostics-button {
  justify-self: start;
}

.diagnostics-output {
  max-height: var(--diagnostics-max-height);
  overflow: auto;
  margin: 0;
  padding: var(--space-3);
  border: 1px solid var(--border-default);
  border-radius: var(--radius-control);
  background: var(--bg-canvas);
  color: var(--text-secondary);
  font-size: var(--font-size-code);
  line-height: var(--line-code);
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}
.health-steps {
  display: grid;
  gap: var(--space-2);
  margin: var(--space-3) 0 0;
  padding: 0;
  list-style: none;
}
.health-steps li {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-2);
}
.health-steps li span {
  flex: 1 1 18rem;
  color: var(--text-secondary);
  font-size: var(--font-size-meta);
}
</style>
