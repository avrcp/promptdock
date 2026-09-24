<script setup lang="ts">
import StatusChip from '@/components/StatusChip.vue'
import TimeAgo from '@/components/TimeAgo.vue'
import CopyValue from '@/components/CopyValue.vue'
import type { WechatAdminStatus } from '@/contracts/wechat'

import type { WechatStatusTone } from './wechat-status'

defineProps<{
  wechat: WechatAdminStatus
  statusVisual: { tone: WechatStatusTone; label: string }
}>()
</script>

<template>
  <section class="wechat-status" aria-labelledby="wechat-status-title">
    <header class="wechat-status__header">
      <div class="wechat-status__identity">
        <h2 id="wechat-status-title" class="wechat-status__title">通道状态</h2>
        <StatusChip :tone="statusVisual.tone" :label="statusVisual.label" />
      </div>
      <div class="wechat-status__header-actions">
        <CopyValue v-if="wechat.accountHint" :value="wechat.accountHint" label="账号提示" />
        <slot name="header-actions" />
      </div>
    </header>

    <h3 class="wechat-status__section-title">运行指标</h3>
    <dl class="wechat-status__details">
      <div class="wechat-status__detail">
        <dt>最近轮询</dt>
        <dd><TimeAgo :timestamp="wechat.lastPollAt" fallback="—" /></dd>
      </div>
      <div class="wechat-status__detail">
        <dt>最近上下文</dt>
        <dd><TimeAgo :timestamp="wechat.lastContextAt" fallback="—" /></dd>
      </div>
      <div class="wechat-status__detail">
        <dt>微信侧接受</dt>
        <dd><TimeAgo :timestamp="wechat.lastProviderAcceptedAt" fallback="—" /></dd>
      </div>
      <div class="wechat-status__detail wechat-status__detail--queue">
        <dt>队列</dt>
        <dd class="tabular">
          {{ wechat.queue.pending }} 待发 · {{ wechat.queue.sending }} 发送中 ·
          {{ wechat.queue.retrying }} 重试 · {{ wechat.queue.blocked }} 阻塞 ·
          {{ wechat.queue.failed }} 失败
        </dd>
      </div>
      <div v-if="wechat.lastErrorCode" class="wechat-status__detail wechat-status__detail--error">
        <dt>最后错误</dt>
        <dd class="mono break-anywhere">{{ wechat.lastErrorCode }}</dd>
      </div>
    </dl>

    <div class="wechat-status__actions">
      <slot />
    </div>
  </section>
</template>

<style scoped>
.wechat-status {
  background: var(--pd-container-panel-bg);
  border: 1px solid var(--pd-border-separator);
  border-radius: var(--pd-radius-md);
  overflow: hidden;
}

.wechat-status__header {
  display: grid;
  grid-template-columns: minmax(max-content, 1fr) minmax(0, auto);
  align-items: center;
  gap: var(--pd-space-16);
  min-height: var(--pd-panel-header-height);
  padding: var(--pd-space-12) var(--pd-space-16);
  border-bottom: 1px solid var(--pd-border-separator);
  background: var(--pd-container-header-bg);
}

.wechat-status__identity {
  display: flex;
  min-width: 0;
  align-items: center;
  gap: var(--pd-space-12);
  flex-wrap: wrap;
}

.wechat-status__header-actions {
  display: flex;
  min-width: 0;
  align-items: center;
  justify-content: flex-end;
  gap: var(--pd-space-8);
}

.wechat-status__header-actions :deep(.copy-value) {
  flex: 1 1 auto;
}

.wechat-status__header-actions :deep(.app-button) {
  flex: 0 0 auto;
}

.wechat-status__title {
  margin: 0;
  color: var(--pd-text-default);
  font-size: var(--pd-font-size-14);
  font-weight: var(--pd-font-weight-semibold);
  line-height: var(--pd-line-height-ui);
}

.wechat-status__section-title {
  margin: 0;
  padding: var(--pd-space-12) var(--pd-space-16);
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-12);
  font-weight: var(--pd-font-weight-medium);
  line-height: var(--pd-line-height-metadata);
}

.wechat-status__details {
  display: grid;
  margin: 0;
  grid-template-columns: repeat(auto-fit, minmax(min(100%, 10rem), 1fr));
}

.wechat-status__detail {
  display: flex;
  flex-direction: column;
  gap: var(--pd-space-4);
  min-width: 0;
  padding: var(--pd-space-12) var(--pd-space-16);
  border-bottom: 1px solid var(--pd-border-separator);
}

.wechat-status__detail dt {
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-12);
  line-height: var(--pd-line-height-metadata);
}

.wechat-status__detail dd {
  margin: 0;
  color: var(--pd-text-default);
  font-size: var(--pd-font-size-13);
  line-height: var(--pd-line-height-ui);
}

.wechat-status__detail--queue {
  grid-column: 1 / -1;
}

.wechat-status__detail--error dd {
  color: var(--pd-feedback-danger);
}

.wechat-status__actions {
  padding: var(--pd-space-16);
}

@media (max-width: 767px) {
  .wechat-status__header {
    align-items: flex-start;
    grid-template-columns: minmax(0, 1fr);
  }

  .wechat-status__header-actions {
    width: 100%;
    justify-content: space-between;
  }
}
</style>
