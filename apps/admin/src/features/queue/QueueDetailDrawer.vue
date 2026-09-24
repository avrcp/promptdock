<script setup lang="ts">
import AppDrawer from '@/components/AppDrawer.vue'
import StatusChip from '@/components/StatusChip.vue'
import TimeAgo from '@/components/TimeAgo.vue'
import {
  deliveryKindVisual,
  deliveryOriginVisual,
  deliveryPriorityVisual,
  deliveryStateVisual,
  inboundCommandVisual,
  inboundStateVisual,
  replyStateVisual,
} from './queue-status'
import type { SelectedQueueRow } from './useQueueController'

defineProps<{
  selected: SelectedQueueRow
  missing: boolean
}>()

const open = defineModel<boolean>('open', { required: true })
</script>

<template>
  <AppDrawer v-model:open="open" title="条目详情">
    <template v-if="selected">
      <div
        v-if="selected.tab === 'deliveries'"
        class="queue-detail"
        role="group"
        aria-label="投递详情"
      >
        <section class="queue-detail__group" aria-labelledby="delivery-overview-title">
          <h3 id="delivery-overview-title">概览</h3>
          <dl class="queue-detail__list">
            <div>
              <dt>类型</dt>
              <dd>{{ deliveryKindVisual(selected.row.kind) }}</dd>
            </div>
            <div>
              <dt>状态</dt>
              <dd>
                <StatusChip
                  :tone="deliveryStateVisual(selected.row.state).tone"
                  :label="deliveryStateVisual(selected.row.state).label"
                />
              </dd>
            </div>
            <div>
              <dt>优先级</dt>
              <dd class="tabular">{{ deliveryPriorityVisual(selected.row.priority) }}</dd>
            </div>
          </dl>
        </section>
        <section class="queue-detail__group" aria-labelledby="delivery-route-title">
          <h3 id="delivery-route-title">路由</h3>
          <dl class="queue-detail__list">
            <div>
              <dt>条目 ID</dt>
              <dd class="queue-detail__mono">{{ selected.row.id }}</dd>
            </div>
            <div>
              <dt>来源</dt>
              <dd class="queue-detail__mono">
                {{ deliveryOriginVisual(selected.row.origin) }} · {{ selected.row.originLabel }}
              </dd>
            </div>
          </dl>
        </section>
        <section class="queue-detail__group" aria-labelledby="delivery-retry-title">
          <h3 id="delivery-retry-title">重试</h3>
          <dl class="queue-detail__list">
            <div>
              <dt>尝试次数</dt>
              <dd class="tabular">{{ selected.row.attemptCount }}</dd>
            </div>
            <div>
              <dt>创建时间</dt>
              <dd><TimeAgo :timestamp="selected.row.createdAt" /></dd>
            </div>
            <div>
              <dt>更新时间</dt>
              <dd><TimeAgo :timestamp="selected.row.updatedAt" /></dd>
            </div>
          </dl>
        </section>
        <section class="queue-detail__group" aria-labelledby="delivery-error-title">
          <h3 id="delivery-error-title">错误</h3>
          <dl class="queue-detail__list">
            <div>
              <dt>错误码</dt>
              <dd class="queue-detail__error mono break-anywhere">
                {{ selected.row.errorCode ?? '—' }}
              </dd>
            </div>
          </dl>
        </section>
      </div>

      <div
        v-else-if="selected.tab === 'replies'"
        class="queue-detail"
        role="group"
        aria-label="交互回复详情"
      >
        <section class="queue-detail__group" aria-labelledby="reply-overview-title">
          <h3 id="reply-overview-title">概览</h3>
          <dl class="queue-detail__list">
            <div>
              <dt>状态</dt>
              <dd>
                <StatusChip
                  :tone="replyStateVisual(selected.row.state).tone"
                  :label="replyStateVisual(selected.row.state).label"
                />
              </dd>
            </div>
            <div>
              <dt>发生时间</dt>
              <dd><TimeAgo :timestamp="selected.row.occurredAt" /></dd>
            </div>
          </dl>
        </section>
        <section class="queue-detail__group" aria-labelledby="reply-route-title">
          <h3 id="reply-route-title">路由</h3>
          <dl class="queue-detail__list">
            <div>
              <dt>条目 ID</dt>
              <dd class="queue-detail__mono">{{ selected.row.id }}</dd>
            </div>
            <div>
              <dt>关联命令</dt>
              <dd class="queue-detail__mono">{{ selected.row.commandRef }}</dd>
            </div>
            <div>
              <dt>目标账号指纹</dt>
              <dd class="queue-detail__mono">{{ selected.row.targetFingerprint }}</dd>
            </div>
          </dl>
        </section>
        <section class="queue-detail__group" aria-labelledby="reply-error-title">
          <h3 id="reply-error-title">错误</h3>
          <dl class="queue-detail__list">
            <div>
              <dt>错误码</dt>
              <dd class="queue-detail__error mono break-anywhere">
                {{ selected.row.errorCode ?? '—' }}
              </dd>
            </div>
          </dl>
        </section>
      </div>

      <div v-else class="queue-detail" role="group" aria-label="入站命令详情">
        <section class="queue-detail__group" aria-labelledby="inbound-overview-title">
          <h3 id="inbound-overview-title">概览</h3>
          <dl class="queue-detail__list">
            <div>
              <dt>命令</dt>
              <dd>{{ inboundCommandVisual(selected.row.command) }}</dd>
            </div>
            <div>
              <dt>状态</dt>
              <dd>
                <StatusChip
                  :tone="inboundStateVisual(selected.row.state).tone"
                  :label="inboundStateVisual(selected.row.state).label"
                />
              </dd>
            </div>
          </dl>
        </section>
        <section class="queue-detail__group" aria-labelledby="inbound-route-title">
          <h3 id="inbound-route-title">路由</h3>
          <dl class="queue-detail__list">
            <div>
              <dt>条目 ID</dt>
              <dd class="queue-detail__mono">{{ selected.row.id }}</dd>
            </div>
            <div>
              <dt>发送者</dt>
              <dd class="queue-detail__mono">{{ selected.row.senderHint }}</dd>
            </div>
          </dl>
        </section>
        <section class="queue-detail__group" aria-labelledby="inbound-retry-title">
          <h3 id="inbound-retry-title">处理时间线</h3>
          <dl class="queue-detail__list">
            <div>
              <dt>尝试次数</dt>
              <dd class="tabular">{{ selected.row.attemptCount }}</dd>
            </div>
            <div>
              <dt>创建时间</dt>
              <dd><TimeAgo :timestamp="selected.row.createdAt" /></dd>
            </div>
            <div>
              <dt>过期时间</dt>
              <dd><TimeAgo :timestamp="selected.row.expiresAt" /></dd>
            </div>
            <div>
              <dt>更新时间</dt>
              <dd><TimeAgo :timestamp="selected.row.updatedAt" /></dd>
            </div>
          </dl>
        </section>
        <section class="queue-detail__group" aria-labelledby="inbound-error-title">
          <h3 id="inbound-error-title">错误</h3>
          <dl class="queue-detail__list">
            <div>
              <dt>最后错误</dt>
              <dd class="queue-detail__error mono break-anywhere">
                {{ selected.row.errorCode ?? '—' }}
              </dd>
            </div>
          </dl>
        </section>
      </div>
    </template>
    <p v-else-if="missing" class="queue-detail__missing" data-testid="queue-item-missing">
      当前页未找到该条目。请返回列表后重试，或检查分享链接是否仍有效。
    </p>
  </AppDrawer>
</template>

<style scoped>
.queue-detail {
  display: flex;
  flex-direction: column;
}

.queue-detail__group {
  padding: var(--pd-space-16) 0;
  border-bottom: 1px solid var(--pd-border-separator);
}

.queue-detail__group:first-child {
  padding-top: 0;
}
.queue-detail__group:last-child {
  padding-bottom: 0;
  border-bottom: 0;
}

.queue-detail__group h3 {
  margin: 0 0 var(--pd-space-12);
  color: var(--pd-text-subtle);
  font-size: var(--pd-font-size-11);
  font-weight: var(--pd-font-weight-semibold);
  line-height: var(--pd-line-height-metadata);
  letter-spacing: 0.04em;
}

.queue-detail__list {
  display: flex;
  flex-direction: column;
  gap: var(--pd-space-8);
  margin: 0;
}

.queue-detail__list > div {
  display: grid;
  grid-template-columns: minmax(88px, 0.75fr) minmax(0, 1.5fr);
  align-items: start;
  gap: var(--pd-space-12);
  min-width: 0;
  font-size: var(--pd-font-size-13);
  line-height: var(--pd-line-height-ui);
}

.queue-detail__list dt {
  color: var(--pd-text-muted);
}
.queue-detail__list dd {
  min-width: 0;
  margin: 0;
  overflow-wrap: anywhere;
  color: var(--pd-text-default);
  text-align: end;
}

.queue-detail__mono {
  font-family: var(--pd-font-code);
  font-size: var(--pd-font-size-11);
  line-height: var(--pd-line-height-code);
}

.queue-detail__list dd.queue-detail__error {
  color: var(--pd-feedback-danger);
  font-size: var(--pd-font-size-11);
}

.queue-detail__missing {
  margin: 0;
  padding-inline-start: var(--pd-space-12);
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-13);
  line-height: var(--pd-line-height-reading);
  border-inline-start: 2px solid var(--pd-feedback-warning);
}
</style>
