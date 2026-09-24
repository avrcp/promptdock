<script setup lang="ts">
import { ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import PanelSection from './PanelSection.vue'
import StatusChip from './StatusChip.vue'
import { trustLabels } from '../desktop/hookHealth'
import { useDesktopState } from '../desktop/desktopState'
defineProps<{ compact?: boolean }>()
const { hookHealth: health, hookHealthPending: pending, hookLabel: summary, refreshHookStatus: refresh } =
  useDesktopState()
const feedback = ref('')
const testing = ref(false)
async function verify() {
  if (pending.value) return
  try {
    await invoke('desktop_hook_verify')
    await refresh()
  } catch (error) {
    feedback.value = typeof error === 'string' ? error : '无法开始验证，请先检查配置。'
  }
}
async function selfTest() {
  if (testing.value) return
  testing.value = true
  try {
    const receipt = await invoke<{ outcome: string }>('desktop_hook_self_test')
    if (receipt?.outcome !== 'local_component_passed') throw new Error('unknown receipt')
    feedback.value = '本地捕获组件自检通过；尚未证明桌面 Hook 实际触发。'
  } catch (e) {
    feedback.value = typeof e === 'string' ? e : '本地组件自检失败，请检查安装文件。'
  } finally {
    testing.value = false
  }
}
async function copyInstruction() {
  if (!health.value?.verification.instruction) return
  try {
    await navigator.clipboard.writeText(health.value.verification.instruction)
    feedback.value = '验证指令已复制，请在实际桌面 Codex 提交。'
  } catch {
    feedback.value = '剪贴板不可用，请手动选择下方指令并复制。'
  }
}
</script>
<template>
  <PanelSection title="Hook 证据与下一步" class="hook-health-panel">
    <div class="stack panel__body">
      <p role="status">{{ summary }}</p>
      <template v-if="health">
        <dl class="summary-list">
          <div>
            <dt>配置安装</dt>
            <dd>
              {{
                {
                  absent: '尚未安装',
                  current: '配置已就绪',
                  needs_repair: '配置需要修复',
                  checking: '正在重新检查',
                  error: '配置暂不可读',
                }[health.installation]
              }}
            </dd>
          </div>
          <div>
            <dt>宿主信任</dt>
            <dd>仅核对持久记录；宿主有效设置与会话覆盖尚未确认</dd>
          </div>
          <div v-if="health.verification.validatedAt">
            <dt>配对时间</dt>
            <dd>
              {{ new Date(health.verification.validatedAt).toLocaleString() }} ·
              共享配置来源未独立鉴别
            </dd>
          </div>
        </dl>
        <template v-if="!compact">
          <ul class="hook-evidence-list">
            <li v-for="handler in health.handlers" :key="handler.event">
              <strong>{{ handler.event }}{{ handler.required ? '（核心）' : '（可选）' }}</strong>
              <span>{{ handler.configured ? '配置就绪' : '未配置或定义不匹配' }}</span>
              <span>{{
                handler.lastObservedAt
                  ? `最近事件：${new Date(handler.lastObservedAt).toLocaleString()}（${handler.observationCurrent ? '当前定义与构建' : '历史证据'}）`
                  : '尚无实际事件'
              }}</span>
              <StatusChip
                :tone="health.fresh && handler.trust === 'matching_record' ? 'success' : 'neutral'"
                :label="trustLabels[handler.trust]"
              />
            </li>
          </ul>
          <p class="field__hint">
            在实际桌面宿主审阅 Hook
            后，可执行一轮验证。验证只关联开始和结束，不发送微信，不证明手机已显示。
          </p>
          <div class="panel__actions">
            <button class="button button--secondary" type="button" @click="() => refresh()">
              检查状态
            </button>
            <button
              class="button button--secondary"
              type="button"
              :disabled="testing"
              @click="selfTest"
            >
              本地组件自检
            </button>
            <button
              class="button button--primary"
              type="button"
              :disabled="
                pending ||
                !health.fresh ||
                !health.observationEnabled ||
                health.installation !== 'current' ||
                !health.registrationId
              "
              @click="verify"
            >
              验证实际触发
            </button>
            <button
              v-if="health.verification.instruction"
              class="button button--secondary"
              type="button"
              @click="copyInstruction"
            >
              复制验证指令
            </button>
          </div>
          <p v-if="feedback" role="status">{{ feedback }}</p>
          <p v-if="health.verification.instruction" class="hook-instruction">
            {{ health.verification.instruction }}
          </p>
          <details>
            <summary>高级：来源与证据范围</summary>
            <dl class="summary-list">
              <div>
                <dt>Hook 声明</dt>
                <dd>{{ health.hookSourcePath || '未选择' }}</dd>
              </div>
              <div>
                <dt>用户信任来源候选</dt>
                <dd>{{ health.userStateSourcePath || '未知' }}</dd>
              </div>
              <div>
                <dt>定义指纹</dt>
                <dd>{{ health.definitionFingerprint || '尚无' }}</dd>
              </div>
              <div>
                <dt>注册标识</dt>
                <dd>{{ health.registrationId || '尚未安装当前注册' }}</dd>
              </div>
              <div>
                <dt>宿主语义</dt>
                <dd>参考源码已固定；实际宿主版本尚未验证</dd>
              </div>
              <div>
                <dt>最近检查</dt>
                <dd>
                  {{ new Date(health.observedAt).toLocaleString() }} {{ health.diagnosticCode }}
                </dd>
              </div>
            </dl>
          </details>
        </template>
      </template>
    </div>
  </PanelSection>
</template>
<style scoped>
.hook-health-panel {
  margin-block-end: var(--space-4);
}
.hook-evidence-list {
  list-style: none;
  padding: 0;
  display: grid;
  gap: var(--space-3);
}
.hook-evidence-list li {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-3);
}
.hook-instruction {
  user-select: text;
  overflow-wrap: anywhere;
}
dd {
  overflow-wrap: anywhere;
}
</style>
