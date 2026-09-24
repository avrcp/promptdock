<script setup lang="ts">
import { ref } from 'vue'
import ConfirmDialog from '../components/ConfirmDialog.vue'
import HookHealthPanel from '../components/HookHealthPanel.vue'
import NavIcon from '../components/NavIcon.vue'
import PanelSection from '../components/PanelSection.vue'
import StatusChip from '../components/StatusChip.vue'
import { useDesktopState } from '../desktop/desktopState'

const {
  status,
  launcher,
  launcherDraft,
  launcherDirty,
  launcherBaselineChanged,
  launcherErrors,
  launcherValid,
  refreshLauncherDiscovery,
  homeDirty,
  homeBaselineChanged,
  hookPlan,
  hookPlanCurrent,
  hookRefreshing,
  home,
  hookLabel,
  hookTone,
  hostLabel,
  hostTone,
  isPending,
  act,
  saveLauncher,
  resetLauncherDraft,
  resetHomeDraft,
  planHook,
  applyHookPlan,
  launchDesktop,
  refreshHookStatus,
} = useDesktopState()

const confirmUninstall = ref(false)

async function uninstallHook() {
  confirmUninstall.value = false
  await act('desktop_uninstall_hook')
}
</script>

<template>
  <div class="view form-view">
    <PanelSection
      title="桌面宿主"
      description="选择实际使用的 ChatGPT 桌面应用。代理只注入新启动的进程。"
    >
      <template #actions>
        <StatusChip :tone="hostTone" :label="hostLabel" />
      </template>

      <div class="form-grid">
        <label class="field field--wide" for="host-path">
          <span class="field__label">已选可执行文件</span>
          <input
            id="host-path"
            v-model="launcherDraft!.desktop.selectedExecutable"
            class="input mono"
            list="host-candidates"
            placeholder="选择你实际使用的桌面应用"
            spellcheck="false"
            :disabled="isPending('launcher')"
            :aria-invalid="launcher?.discoveryIssue || launcher?.selectionIssue ? 'true' : undefined"
            aria-describedby="host-path-error"
          />
        </label>
        <datalist id="host-candidates">
          <option
            v-for="candidate in launcher?.candidates ?? []"
            :key="candidate.executable"
            :value="candidate.executable"
          >
            {{ candidate.productLabel }} {{ candidate.packageVersion }}
          </option>
        </datalist>
        <p v-if="launcher?.discoveryIssue" id="host-path-error" class="field-error" role="status">
          {{ launcher.discoveryIssue.message }}
        </p>
        <p v-if="launcher?.selectionIssue" class="field-error" role="status">
          {{ launcher.selectionIssue.message }} 请从上方候选中选择当前实际使用的桌面应用后保存。
        </p>
        <label class="toggle-row field--wide">
          <input
            v-model="launcherDraft!.proxy.enabled"
            type="checkbox"
            :disabled="isPending('launcher')"
          />
          <span class="toggle-row__text">
            <strong class="toggle-row__title">启动时注入本地代理</strong>
            <small class="toggle-row__hint"
              >只影响由 PromptDock 新启动的桌面进程，不修改系统代理。</small
            >
          </span>
        </label>
        <label class="field" for="proxy-host">
          <span class="field__label">本地代理地址</span>
          <input
            id="proxy-host"
            v-model="launcherDraft!.proxy.host"
            class="input"
            :disabled="isPending('launcher')"
            :aria-invalid="launcherErrors.proxyHost ? 'true' : undefined"
            aria-describedby="proxy-host-error"
          />
          <small
            v-if="launcherErrors.proxyHost"
            id="proxy-host-error"
            class="field-error"
            role="status"
          >
            {{ launcherErrors.proxyHost }}
          </small>
        </label>
        <label class="field" for="proxy-port">
          <span class="field__label">端口</span>
          <input
            id="proxy-port"
            v-model.number="launcherDraft!.proxy.port"
            class="input numeric"
            type="number"
            min="1"
            max="65535"
            :disabled="isPending('launcher')"
            :aria-invalid="launcherErrors.proxyPort ? 'true' : undefined"
            aria-describedby="proxy-port-error"
          />
          <small
            v-if="launcherErrors.proxyPort"
            id="proxy-port-error"
            class="field-error"
            role="status"
          >
            {{ launcherErrors.proxyPort }}
          </small>
        </label>
        <p v-if="launcherBaselineChanged" class="form-notice field--wide" role="status">
          后台保存的启动设置已变化；你的输入仍保留。保存会基于原版本校验，或还原后重新编辑。
        </p>
      </div>

      <div class="panel__actions">
        <button
          class="button button--secondary"
          type="button"
          :disabled="isPending('launcher')"
          @click="refreshLauncherDiscovery"
        >
          重新发现桌面应用
        </button>
        <button
          class="button button--secondary"
          type="button"
          :disabled="isPending('launcher') || !launcherDirty || !launcherValid"
          @click="saveLauncher"
        >
          保存启动设置
        </button>
        <button
          v-if="launcherDirty"
          class="button button--ghost"
          type="button"
          :disabled="isPending('launcher')"
          @click="resetLauncherDraft"
        >
          还原启动设置
        </button>
        <button
          class="button button--primary"
          type="button"
          :disabled="
            isPending('launcher') || !launcherDraft?.desktop.selectedExecutable || !launcherValid
          "
          @click="launchDesktop"
        >
          <NavIcon name="launch" />{{
            launcherDirty ? '保存并启动 ChatGPT 桌面端' : '启动 ChatGPT 桌面端'
          }}
        </button>
      </div>
    </PanelSection>

    <HookHealthPanel />

    <PanelSection
      title="Codex Hook 配置"
      description="共享此配置目录的其他 Codex 客户端也可能触发；持久信任记录与实际接收事实分别显示。"
    >
      <template #actions>
        <StatusChip :tone="hookTone" :label="hookLabel" />
      </template>

      <div class="stack panel__body">
        <label class="field" for="hook-home">
          <span class="field__label">目标 Codex 配置目录</span>
          <input
            id="hook-home"
            v-model="home"
            class="input mono"
            placeholder="显式选择实际生效的 Codex 配置目录"
            spellcheck="false"
            :disabled="isPending('hook')"
          />
        </label>
        <p v-if="homeBaselineChanged" class="form-notice" role="status">
          后台记录的配置目录已变化；你的输入仍保留。
        </p>
        <section v-if="hookPlanCurrent && hookPlan" class="hook-plan" aria-label="Hook 变更计划">
          <strong>{{
            hookPlan.outcome === 'no_change' ? '当前定义无需改动' : '准备应用 Hook 变更'
          }}</strong>
          <span v-if="hookPlan.changedEvents.length">
            受影响事件：{{ hookPlan.changedEvents.join('、') }}
          </span>
          <span v-else>没有 Handler 定义变化。</span>
          <span v-if="hookPlan.reviewRequired === true">
            应用后需要在桌面宿主中重新审阅受影响的 Hook。
          </span>
          <span v-else-if="hookPlan.reviewRequired === null">是否需要重新审阅暂不能确认。</span>
          <span v-if="hookPlan.externalReviewRequired" class="hook-plan__external">
            外部 Hook 的位置也会受影响；应用后请一并重新审阅这些外部定义。
          </span>
        </section>
        <p class="panel__note panel__note--flush">
          接收状态：{{ status?.inboxPresent ? '已有接收文件' : '尚未收到事件' }}
        </p>
      </div>

      <div class="panel__actions">
        <button
          class="button button--secondary"
          type="button"
          :disabled="hookRefreshing"
          :aria-busy="hookRefreshing"
          @click="refreshHookStatus()"
        >
          <NavIcon name="refresh" />{{ hookRefreshing ? '正在刷新' : '刷新状态' }}
        </button>
        <button
          class="button button--primary"
          type="button"
          :disabled="isPending('hook') || !home.trim()"
          @click="planHook"
        >
          检查变更
        </button>
        <button
          v-if="hookPlanCurrent"
          class="button button--primary"
          type="button"
          :disabled="isPending('hook')"
          @click="applyHookPlan"
        >
          {{ hookPlan?.outcome === 'no_change' ? '确认当前配置' : '应用 Hook 变更' }}
        </button>
        <button
          v-if="homeDirty"
          class="button button--ghost"
          type="button"
          :disabled="isPending('hook')"
          @click="resetHomeDraft"
        >
          还原目录
        </button>
        <button
          class="button button--danger"
          type="button"
          :disabled="isPending('hook') || !status?.hookHome"
          @click="confirmUninstall = true"
        >
          卸载
        </button>
      </div>
    </PanelSection>

    <ConfirmDialog
      :open="confirmUninstall"
      title="卸载 Codex Hook？"
      description="卸载后本机不再接收 Codex 会话事件，需要重新安装才能恢复。"
      confirm-label="卸载 Hook"
      @cancel="confirmUninstall = false"
      @confirm="uninstallHook"
    />
  </div>
</template>

<style scoped>
.form-view {
  display: grid;
  gap: var(--space-3);
}

.form-grid {
  display: grid;
  grid-template-columns: var(--form-grid-columns);
  gap: var(--form-grid-gap);
  padding: var(--panel-padding);
}

.field--wide {
  grid-column: 1 / -1;
}

.panel__note--flush {
  padding: 0;
}

.form-notice {
  margin: 0;
  color: var(--status-warning);
  font-size: var(--font-size-meta);
  line-height: var(--line-body);
}

.hook-plan {
  display: grid;
  gap: var(--space-1);
  padding: var(--space-3);
  border: 1px solid var(--warning-border);
  border-radius: var(--radius-control);
  background: var(--warning-soft);
  color: var(--text-secondary);
  font-size: var(--font-size-meta);
  line-height: var(--line-body);
}

.hook-plan strong {
  color: var(--text-primary);
}

.hook-plan__external {
  color: var(--status-warning);
}

/* 断点：CSS 媒体查询无法插值自定义属性，此处为唯一声明点 */
@media (max-width: 720px) {
  .form-grid {
    grid-template-columns: minmax(0, 1fr);
  }

  .field--wide {
    grid-column: auto;
  }
}
</style>
