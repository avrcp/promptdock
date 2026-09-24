<script setup lang="ts">
import AppDialog from '@/components/AppDialog.vue'
import InlineAlert from '@/components/InlineAlert.vue'
import { runtimeEnvironment } from '@/app/environment'
import type { DeviceScope } from '@/contracts/device'
import type { AdminError } from '@/contracts/error'

defineProps<{
  inFlight: boolean
  error: AdminError | null
  canManage: boolean
}>()

const open = defineModel<boolean>('open', { required: true })
const name = defineModel<string>('name', { required: true })
const scopes = defineModel<DeviceScope[]>('scopes', { required: true })

const scopeOptions: ReadonlyArray<{
  value: DeviceScope
  label: string
}> = [
  { value: 'gateway:connect', label: '建立 Gateway 长连接' },
  { value: 'notify:write', label: '提交通知任务' },
  { value: 'notify:read_own', label: '读取本设备通知状态' },
  { value: 'channel:read', label: '读取通道状态' },
  { value: 'channel:manage', label: '管理通道' },
  { value: 'job:query', label: '查询任务' },
  { value: 'job:control', label: '控制任务' },
]

const emit = defineEmits<{ submit: [] }>()
</script>

<template>
  <AppDialog
    v-model:open="open"
    title="新建设备"
    :description="
      runtimeEnvironment.mode === 'mock'
        ? 'Mock 操作不会调用真实 API。设备创建后会出现在设备列表中。'
        : '设备创建后会立即显示一次性 Token，请复制保存到安全位置。'
    "
    primary-label="创建并显示 Token"
    :primary-loading="inFlight"
    :primary-disabled="!canManage"
    :pending="inFlight"
    @primary="emit('submit')"
  >
    <div class="device-create__form">
      <label class="device-create__name">
        <span>设备名称</span>
        <input
          v-model="name"
          type="text"
          maxlength="64"
          placeholder="例如：主控工作站"
          data-testid="device-create-name"
          :aria-describedby="error ? 'device-create-error' : undefined"
        />
      </label>
      <fieldset
        class="device-create__scopes"
        data-testid="device-create-scopes"
        :disabled="inFlight"
        :aria-describedby="
          error ? 'device-create-scopes-hint device-create-error' : 'device-create-scopes-hint'
        "
      >
        <legend>授权范围</legend>
        <p id="device-create-scopes-hint">只授予该设备实际需要的权限。</p>
        <div class="device-create__scope-grid">
          <label v-for="option in scopeOptions" :key="option.value">
            <input
              v-model="scopes"
              type="checkbox"
              :value="option.value"
              :data-scope="option.value"
            />
            <span>
              <code>{{ option.value }}</code>
              <small>{{ option.label }}</small>
            </span>
          </label>
        </div>
      </fieldset>
      <InlineAlert
        v-if="error"
        id="device-create-error"
        tone="danger"
        :title="error.message"
        :code="error.code"
      />
    </div>
  </AppDialog>
</template>

<style scoped>
.device-create__form {
  display: flex;
  flex-direction: column;
  gap: var(--pd-space-16);
}

.device-create__name {
  display: flex;
  flex-direction: column;
  gap: var(--pd-space-4);
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-13);
  font-weight: var(--pd-font-weight-medium);
}

.device-create__name input {
  height: var(--pd-control-height-md);
  padding: 0 var(--pd-space-8);
  background: var(--pd-control-bg);
  color: var(--pd-text-default);
  border: 1px solid var(--pd-control-border-default);
  border-radius: var(--pd-radius-sm);
  font-size: max(16px, var(--pd-font-size-13));
}

.device-create__name input:focus-visible {
  border-color: var(--pd-border-focus);
  outline: 2px solid var(--pd-border-focus);
  outline-offset: 0;
}

.device-create__scopes {
  min-width: 0;
  margin: 0;
  padding: 0;
  border: 0;
}

.device-create__scopes legend {
  padding: 0;
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-13);
  font-weight: var(--pd-font-weight-semibold);
}

.device-create__scopes > p {
  margin: var(--pd-space-4) 0 var(--pd-space-8);
  color: var(--pd-text-subtle);
  font-size: var(--pd-font-size-12);
}

.device-create__scope-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(min(100%, 14rem), 1fr));
  gap: var(--pd-space-8);
}

.device-create__scope-grid label {
  display: flex;
  align-items: center;
  gap: var(--pd-space-8);
  min-height: var(--pd-control-height-touch);
  padding: var(--pd-space-8);
  border: 1px solid var(--pd-control-border-default);
  border-radius: var(--pd-radius-sm);
  background: var(--pd-control-bg);
  color: var(--pd-text-default);
  cursor: pointer;
}

.device-create__scope-grid label:has(input:checked) {
  border-color: var(--pd-border-emphasis);
  background: var(--pd-control-bg-selected);
}

.device-create__scope-grid input {
  width: 16px;
  height: 16px;
  margin: 0;
  accent-color: var(--pd-primary);
}

.device-create__scope-grid label:has(input:focus-visible) {
  outline: 2px solid var(--pd-border-focus);
  outline-offset: 1px;
}

.device-create__scope-grid span {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: var(--pd-space-2);
}

.device-create__scope-grid code {
  overflow-wrap: anywhere;
  font-size: var(--pd-font-size-12);
}

.device-create__scope-grid small {
  color: var(--pd-text-subtle);
  font-size: var(--pd-font-size-12);
  font-weight: var(--pd-font-weight-regular);
}
</style>
