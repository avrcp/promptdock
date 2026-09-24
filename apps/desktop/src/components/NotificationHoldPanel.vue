<script setup lang="ts">
import { computed } from 'vue'
import StatusChip from './StatusChip.vue'
import { useNotificationHoldController } from '../desktop/notificationHold'

const { status, pending, error, hold, resume } = useNotificationHoldController()
const label = computed(() =>
  !status.value || error.value
    ? '状态待确认'
    : status.value.state === 'active'
      ? '推送暂缓中'
      : status.value.state === 'uncertain'
        ? '状态待确认'
        : '未暂缓推送',
)
const tone = computed(() =>
  !status.value
    ? 'neutral'
    : error.value || status.value.state === 'uncertain'
      ? 'danger'
      : status.value.state === 'active'
        ? 'warning'
        : 'success',
)
const until = computed(() =>
  status.value?.until ? new Date(status.value.until).toLocaleString() : null,
)
</script>
<template>
  <section class="notification-hold" aria-label="临时通知暂缓">
    <div class="notification-hold__heading">
      <div>
        <strong>临时暂缓通知</strong>
        <p>不改变永久通知策略。</p>
      </div>
      <StatusChip :tone="tone" :label="label" />
    </div>
    <p>
      按已选内容策略继续保存本机记录；新的结果页会在恢复后发布。已提交的请求和已接管通知可能继续到达。
    </p>
    <p v-if="until" class="field__hint">暂缓至 {{ until }}。</p>
    <div class="panel__actions">
      <template v-if="status?.state === 'inactive'">
        <button
          class="button button--secondary"
          type="button"
          :disabled="pending"
          @click="hold(15)"
        >
          暂缓 15 分钟
        </button>
        <button
          class="button button--secondary"
          type="button"
          :disabled="pending"
          @click="hold(60)"
        >
          暂缓 60 分钟
        </button>
      </template>
      <button
        v-else
        class="button button--primary"
        type="button"
        :disabled="pending || !status"
        @click="resume"
      >
        恢复通知
      </button>
    </div>
    <p v-if="error" class="field-error" role="status">{{ error }}</p>
  </section>
</template>
<style scoped>
.notification-hold {
  display: grid;
  gap: var(--space-2);
  padding: var(--panel-padding);
  border-top: 1px solid var(--border-default);
}
.notification-hold__heading {
  display: flex;
  flex-wrap: wrap;
  align-items: start;
  justify-content: space-between;
  gap: var(--space-2);
}
.notification-hold__heading p,
.notification-hold > p {
  margin: 0;
  color: var(--text-secondary);
  font-size: var(--font-size-meta);
  line-height: var(--line-body);
}
</style>
