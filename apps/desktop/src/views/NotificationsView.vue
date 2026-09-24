<script setup lang="ts">
import PanelSection from '../components/PanelSection.vue'
import StatusChip from '../components/StatusChip.vue'
import NotificationHoldPanel from '../components/NotificationHoldPanel.vue'
import { useDesktopState } from '../desktop/desktopState'

const {
  status,
  relay,
  relayUrl,
  deviceToken,
  policyDraft,
  policyDirty,
  policyBaselineChanged,
  policyErrors,
  policyValid,
  relayLabel,
  relayTone,
  isPending,
  act,
  configureRelay,
  savePolicy,
  resetPolicyDraft,
} = useDesktopState()
</script>

<template>
  <div class="view notification-settings">
    <PanelSection
      title="Relay 连接"
      description="服务器管理微信连接；设备凭据仅存入本机受保护存储。"
    >
      <template #actions>
        <StatusChip :tone="relayTone" :label="relayLabel" />
      </template>

      <div class="stack panel__body">
        <label class="field" for="relay-url">
          <span class="field__label">服务器地址</span>
          <input
            id="relay-url"
            v-model="relayUrl"
            class="input"
            type="url"
            placeholder="https://你的通知服务器"
            spellcheck="false"
            :disabled="isPending('relay')"
            :aria-invalid="relay?.lastErrorCode ? 'true' : undefined"
            aria-describedby="relay-url-hint relay-url-error"
          />
          <small id="relay-url-hint" class="field__hint"
            >公网地址使用 HTTPS；本机验收可使用 127.0.0.1 或 ::1 的 HTTP 地址。</small
          >
        </label>

        <label class="field" for="device-token">
          <span class="field__label">新设备 Token</span>
          <input
            id="device-token"
            v-model="deviceToken"
            class="input"
            type="password"
            autocomplete="off"
            spellcheck="false"
            :disabled="isPending('relay')"
            aria-describedby="device-token-hint"
          />
          <small id="device-token-hint" class="field__hint"
            >在服务器 Admin 登记新设备并授予通知权限。</small
          >
        </label>

        <p v-if="relay?.lastErrorCode" id="relay-url-error" class="field-error" role="status">
          连接错误：{{ relay.lastErrorCode }}。请检查服务器地址与设备 Token 后重新验证。
        </p>
      </div>

      <div class="panel__actions">
        <button
          class="button button--primary"
          type="button"
          :disabled="isPending('relay') || !relayUrl || !deviceToken"
          @click="configureRelay"
        >
          验证并保存
        </button>
        <button
          class="button button--secondary"
          type="button"
          :disabled="isPending('relay') || !relay?.configured"
          @click="act('desktop_relay_probe')"
        >
          检查连接
        </button>
        <button
          class="button button--secondary"
          type="button"
          :disabled="isPending('relay') || !relay?.authenticated"
          @click="act('desktop_relay_test')"
        >
          发送测试通知
        </button>
      </div>

      <template #note> Relay 已接管不代表手机已显示；最终显示仍需在手机上核验。 </template>
    </PanelSection>

    <NotificationHoldPanel />

    <PanelSection
      title="通知与内容"
      description="默认仅发送状态。涉及正文的选项需要明确开启。"
    >
      <div class="policy-list">
        <label class="toggle-row">
          <input
            id="policy-observe"
            v-model="policyDraft!.observe_turns"
            type="checkbox"
            :disabled="isPending('policy')"
          />
          <span class="toggle-row__text">
            <strong class="toggle-row__title">接收 Codex 轮次事件</strong>
            <small class="toggle-row__hint">关闭后不再处理新的会话轮次。</small>
          </span>
        </label>
        <label class="toggle-row">
          <input
            id="policy-started"
            v-model="policyDraft!.notify_started"
            type="checkbox"
            :disabled="isPending('policy')"
          />
          <span class="toggle-row__text">
            <strong class="toggle-row__title">通知本轮开始</strong>
            <small class="toggle-row__hint">Codex 开始处理用户提交时发送状态。</small>
          </span>
        </label>
        <label class="toggle-row">
          <input
            id="policy-ended"
            v-model="policyDraft!.notify_ended"
            type="checkbox"
            :disabled="isPending('policy')"
          />
          <span class="toggle-row__text">
            <strong class="toggle-row__title">通知本轮结束</strong>
            <small class="toggle-row__hint">轮次停止时发送结束状态，不推断任务成功。</small>
          </span>
        </label>
        <label class="field" for="policy-completion-quiet-ms">
          <span class="field__label">结束通知静默窗口（毫秒）</span>
          <input
            id="policy-completion-quiet-ms"
            v-model.number="policyDraft!.completion_quiet_ms"
            class="input"
            type="number"
            min="500"
            max="20000"
            step="100"
            :disabled="isPending('policy')"
            :aria-invalid="policyErrors.completionQuietMs ? 'true' : undefined"
            aria-describedby="policy-completion-quiet-ms-hint"
          />
          <small id="policy-completion-quiet-ms-hint" class="field__hint"
            >等待相邻结束事件合并，范围 500–20000 毫秒，默认 2000 毫秒。</small
          >
          <small v-if="policyErrors.completionQuietMs" class="field-error" role="status">
            {{ policyErrors.completionQuietMs }}
          </small>
        </label>
        <label class="field" for="policy-content-mode">
          <span class="field__label">结束通知正文</span>
          <select
            id="policy-content-mode"
            v-model="policyDraft!.result_content_mode"
            class="input"
            :disabled="isPending('policy')"
            aria-describedby="policy-content-hint"
          >
            <option value="status_only">仅状态</option>
            <option value="redacted_excerpt">脱敏摘录</option>
            <option value="full_final">完整原文（服务器结果页）</option>
          </select>
          <small id="policy-content-hint" class="field__hint">
            <template v-if="policyDraft!.result_content_mode === 'full_final'">
              完整结果保存于你的 Relay，微信发送详情链接。链接有效期内，持有链接的人可以阅读该结果，
              包括代码、路径和其他原文。页面默认有效 7 天，由管理员配置。
              本机正文加密保存；更改设置影响后续结果，已发布链接需在投递详情中单独撤销。
            </template>
            <template v-else-if="policyDraft!.result_content_mode === 'redacted_excerpt'">
              仅发送经过脱敏和截断的摘录，不保证包含完整回答。
            </template>
            <template v-else>不采集或发送回答正文，仅通知本轮状态。</template>
          </small>
        </label>
        <label class="toggle-row">
          <input
            id="policy-include-task-input"
            v-model="policyDraft!.include_task_input"
            type="checkbox"
            :disabled="isPending('policy')"
          />
          <span class="toggle-row__text">
            <strong class="toggle-row__title">在开始通知中包含任务输入</strong>
            <small class="toggle-row__hint"
              >此开关只控制开始通知中的任务输入；结束回答正文仍由上方设置单独控制。</small
            >
          </span>
        </label>
        <label class="toggle-row">
          <input
            id="policy-attention"
            v-model="policyDraft!.notify_attention"
            type="checkbox"
            :disabled="isPending('policy')"
          />
          <span class="toggle-row__text">
            <strong class="toggle-row__title">需要处理时提醒</strong>
            <small class="toggle-row__hint">更改后需要修复 Hook 才会生效。</small>
          </span>
        </label>
      </div>

      <p v-if="policyBaselineChanged" class="policy-notice" role="status">
        后台保存的通知设置已变化；你的输入仍保留。保存会先核对原版本，或还原后重新编辑。
      </p>
      <p v-if="status?.policyApplyStatus === 'pending'" class="policy-notice" role="status">
        已保存的内容策略正在应用；相关事件会等待，不会按旧权限继续外发。
      </p>

      <div class="panel__actions">
        <button
          class="button button--primary"
          type="button"
          :disabled="isPending('policy') || !policyDirty || !policyValid"
          @click="savePolicy"
        >
          保存通知设置
        </button>
        <button
          v-if="policyDirty"
          class="button button--ghost"
          type="button"
          :disabled="isPending('policy')"
          @click="resetPolicyDraft"
        >
          还原通知设置
        </button>
      </div>
    </PanelSection>
  </div>
</template>

<style scoped>
.notification-settings {
  display: grid;
  grid-template-columns: minmax(0, 0.9fr) minmax(0, 1.1fr);
  gap: var(--space-3);
  align-items: start;
}

.policy-list {
  display: grid;
  gap: var(--space-2);
  padding: var(--panel-padding);
}

.policy-notice {
  margin: 0;
  padding: 0 var(--panel-padding) var(--space-2);
  color: var(--status-warning);
  font-size: var(--font-size-meta);
  line-height: var(--line-body);
}

/* 断点：CSS 媒体查询无法插值自定义属性，此处为唯一声明点 */
@media (max-width: 980px) {
  .notification-settings {
    grid-template-columns: minmax(0, 1fr);
  }
}
</style>
