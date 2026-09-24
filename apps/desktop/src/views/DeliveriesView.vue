<script setup lang="ts">
import { computed, onUnmounted, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import ConfirmDialog from '../components/ConfirmDialog.vue'
import DeliveryTable from '../components/DeliveryTable.vue'
import EmptyState from '../components/EmptyState.vue'
import NavIcon from '../components/NavIcon.vue'
import PanelSection from '../components/PanelSection.vue'
import { useDesktopState } from '../desktop/desktopState'
import {
  deliveryLabel,
  resultPageLabel,
  type Delivery,
  type ResultPublication,
} from '../desktop/dictionaries'

const {
  deliveries,
  deliveriesCapped,
  deliveriesNextCursor,
  deliveriesPagePending,
  navigate,
  refreshDeliveries,
  loadMoreDeliveries,
} = useDesktopState()

type Detail = {
  id: string
  title: string
  body: string | null
  contentMode: string
  contentBytes: number
  sourceHash: string | null
  unavailableReason: string | null
}

const selectedId = ref<string | null>(null)
const selectedTitle = ref('')
const selectedPublication = ref<ResultPublication | null>(null)
const detail = ref<Detail | null>(null)
const detailBusy = ref(false)
const detailMessage = ref('')
const actionBusy = ref<'open' | 'copy' | 'revoke' | 'resend' | null>(null)
const actionMessage = ref('')
const revokeConfirmationOpen = ref(false)
const currentTime = ref(Date.now())
const expiryTimer = setInterval(() => {
  currentTime.value = Date.now()
}, 30_000)
let selectionGeneration = 0

const pageAvailable = computed(
  () =>
    selectedPublication.value?.pageState === 'available' &&
    (selectedPublication.value.pageExpiresAt ?? 0) > currentTime.value,
)

function selectMetadata(item: Delivery) {
  selectionGeneration++
  selectedId.value = item.id
  selectedTitle.value = item.payload.title
  selectedPublication.value = item.result ? { ...item.result } : null
  detail.value = detail.value?.id === item.id ? detail.value : null
  detailMessage.value = ''
  actionMessage.value = ''
  void refreshSelectedMetadata(selectionGeneration)
}

async function refreshSelectedMetadata(generation: number) {
  if (!selectedId.value) return
  const id = selectedId.value
  try {
    const item = await invoke<Delivery>('desktop_delivery_metadata', { id })
    if (generation !== selectionGeneration || selectedId.value !== id) return
    selectedTitle.value = item.payload.title
    selectedPublication.value = item.result ? { ...item.result } : null
  } catch {
    // The row snapshot remains usable. Metadata refresh is independent from
    // opening a server result or reading the encrypted local body.
  }
}

function closeSelection() {
  selectionGeneration++
  selectedId.value = null
  selectedTitle.value = ''
  selectedPublication.value = null
  detail.value = null
  detailBusy.value = false
  detailMessage.value = ''
  actionMessage.value = ''
  revokeConfirmationOpen.value = false
}

async function openResult(item: Delivery) {
  selectMetadata(item)
  await resultAction('open')
}

async function openLocalBody(item: Delivery) {
  selectMetadata(item)
  const generation = selectionGeneration
  const id = item.id
  detailBusy.value = true
  detailMessage.value = ''
  try {
    const next = await invoke<Detail>('desktop_delivery_detail', { id })
    if (generation === selectionGeneration && selectedId.value === id) detail.value = next
  } catch {
    if (generation === selectionGeneration && selectedId.value === id) {
      detail.value = null
      detailMessage.value = '本地原文暂不可读取。服务器结果页与链接操作仍可独立使用。'
    }
  } finally {
    if (generation === selectionGeneration) detailBusy.value = false
  }
}

function errorCode(error: unknown): string | null {
  if (typeof error === 'object' && error && 'code' in error) {
    const code = (error as { code?: unknown }).code
    if (typeof code === 'string') return code
  }
  const message = error instanceof Error ? error.message : String(error)
  return /^([A-Z][A-Z0-9_]+):/.exec(message)?.[1] ?? null
}

function actionFailure(action: 'open' | 'copy' | 'revoke' | 'resend', error: unknown) {
  const code = errorCode(error)
  if (action === 'copy') return '复制未完成。请检查网络与剪贴板权限，或选择“查看结果”。'
  if (code === 'RESULT_DESTINATION_CHANGED')
    return '此结果属于先前的 Relay 设备，已阻止转投当前设备。'
  if (
    ['RELAY_AUTH_FAILED', 'RELAY_TOKEN_INVALID', 'RELAY_INSUFFICIENT_SCOPE'].includes(code ?? '')
  )
    return '当前设备凭据没有执行此操作的权限，请在 Relay 与通知中更新凭据。'
  if (['RELAY_RESULT_NOT_FOUND', 'RESULT_NOT_PUBLISHED'].includes(code ?? ''))
    return '服务器已确认此结果或链接不可用，重试不会恢复它。'
  return '操作结果尚未确认。检查连接后可重试；后台会沿用同一次操作身份。'
}

async function resultAction(action: 'open' | 'copy' | 'revoke' | 'resend') {
  if (!selectedId.value || !pageAvailable.value || actionBusy.value) return
  const id = selectedId.value
  const generation = selectionGeneration
  actionBusy.value = action
  actionMessage.value = ''
  try {
    if (action === 'copy') {
      const link = await invoke<{ url: string; expiresAt: number }>('desktop_result_link', { id })
      if (generation !== selectionGeneration) return
      await navigator.clipboard.writeText(link.url)
    } else if (action === 'open') {
      await invoke('desktop_result_open', { id })
    } else {
      // The backend persists the first pending request identity and destination.
      // A fresh candidate UUID here is replaced with that durable identity when
      // an earlier response is still unknown.
      const requestId = crypto.randomUUID()
      const commands = { resend: 'desktop_result_resend', revoke: 'desktop_result_revoke' } as const
      await invoke(commands[action], { id, requestId })
    }
    if (generation === selectionGeneration) {
      actionMessage.value = {
        open: '已在浏览器打开结果页。',
        copy: '链接已复制，请仅分享给需要阅读的人。',
        revoke: '链接已撤销；微信中的旧通知不会消失。',
        resend: '重新通知已由服务器接管，请查看微信通知状态。',
      }[action]
      if (action === 'revoke') revokeConfirmationOpen.value = false
      await refreshDeliveries(false)
      await refreshSelectedMetadata(generation)
    }
  } catch (error) {
    if (generation === selectionGeneration) actionMessage.value = actionFailure(action, error)
  } finally {
    if (generation === selectionGeneration) actionBusy.value = null
  }
}

async function exportDetail() {
  if (!detail.value || detail.value.body === null || detailBusy.value) return
  const generation = selectionGeneration
  detailBusy.value = true
  try {
    const path = await invoke<string>('desktop_export_delivery', { id: detail.value.id })
    if (generation === selectionGeneration) detailMessage.value = `已导出原文：${path}`
  } catch {
    if (generation === selectionGeneration)
      detailMessage.value = '导出失败，请确认本机目录可写后重试。'
  } finally {
    if (generation === selectionGeneration) detailBusy.value = false
  }
}

onUnmounted(() => {
  clearInterval(expiryTimer)
  closeSelection()
})
</script>

<template>
  <div class="view">
    <PanelSection
      title="投递记录"
      description="这里区分本机提交、Relay 接管和微信接口接收；手机显示始终需要人工核验。"
    >
      <template #actions>
        <button
          class="button button--secondary"
          type="button"
          :disabled="deliveriesPagePending"
          @click="refreshDeliveries()"
        >
          <NavIcon name="refresh" />刷新记录
        </button>
      </template>

      <DeliveryTable
        v-if="deliveries.length"
        :items="deliveries"
        :selected-id="selectedId"
        caption="投递记录"
        show-id
        @open-result="openResult"
        @local-body="openLocalBody"
      />
      <EmptyState
        v-else
        icon="deliveries"
        large
        title="暂无投递"
        description="等待 Codex 会话触发，或从 Relay 页面发送测试通知。"
        action-label="前往 Relay 与通知"
        @action="navigate('notifications')"
      />
      <div v-if="deliveries.length" class="panel__actions pagination-actions">
        <p class="field__hint">已显示 {{ deliveries.length }} 条投递记录。</p>
        <p v-if="deliveriesCapped" class="field__hint" role="status">
          已达到本地列表上限；刷新记录后可重新从最近项查看，当前详情不会关闭。
        </p>
        <button
          v-if="deliveriesNextCursor"
          class="button button--secondary"
          type="button"
          :disabled="deliveriesPagePending"
          @click="loadMoreDeliveries"
        >
          {{ deliveriesPagePending ? '正在读取…' : '加载更多' }}
        </button>
        <p v-else class="field__hint">已到最早一条。</p>
      </div>
    </PanelSection>

    <PanelSection
      v-if="selectedId"
      :title="selectedTitle || '投递结果'"
      description="服务器结果、本地元数据和本地原文分别读取；其中一项不可用不会阻断其他入口。"
    >
      <template #actions>
        <button type="button" class="button button--secondary" @click="closeSelection">
          关闭详情
        </button>
      </template>
      <div class="panel__body stack">
        <section v-if="selectedPublication" class="result-summary" aria-label="服务器结果页">
          <div>
            <strong>服务器结果页</strong>
            <p class="field__hint">{{ resultPageLabel(selectedPublication) }}</p>
          </div>
          <div>
            <strong>微信通知</strong>
            <p class="field__hint">
              {{ deliveryLabel(selectedPublication.notificationStatus) }}
            </p>
          </div>
          <div class="result-actions">
            <button
              type="button"
              class="button button--primary"
              :disabled="!!actionBusy || !pageAvailable"
              @click="resultAction('open')"
            >
              查看结果
            </button>
            <button
              type="button"
              class="button button--secondary"
              :disabled="!!actionBusy || !pageAvailable"
              @click="resultAction('copy')"
            >
              复制链接
            </button>
            <button
              type="button"
              class="button button--secondary"
              :disabled="!!actionBusy || !pageAvailable"
              @click="resultAction('resend')"
            >
              重新通知
            </button>
            <button
              type="button"
              class="button button--danger"
              :disabled="!!actionBusy || !pageAvailable"
              @click="revokeConfirmationOpen = true"
            >
              撤销链接
            </button>
          </div>
          <p v-if="!pageAvailable" class="field__hint">
            结果页尚未就绪或已失效，当前无法获取链接或重新通知。
          </p>
          <p v-if="actionBusy" role="status">正在联系服务器…</p>
          <p v-if="actionMessage" role="status">{{ actionMessage }}</p>
        </section>
        <section class="local-body" aria-label="本地原文">
          <div class="local-body__heading">
            <strong>本地原文</strong>
            <small v-if="detail" class="field__hint">{{ detail.contentBytes }} 字节</small>
          </div>
          <p v-if="detailBusy" role="status">正在读取本地原文…</p>
          <p v-if="detailMessage" class="field__hint" role="status">{{ detailMessage }}</p>
          <template v-if="detail">
            <pre v-if="detail.body !== null" class="delivery-body mono" aria-label="通知正文">{{
              detail.body
            }}</pre>
            <p v-else role="status">
              {{ detail.unavailableReason ?? '此记录没有可读取的本地原文。' }}
            </p>
            <button
              type="button"
              class="button button--secondary"
              :disabled="detailBusy || detail.body === null"
              @click="exportDetail"
            >
              导出原文
            </button>
          </template>
          <p v-else-if="!detailBusy && !detailMessage" class="field__hint">
            仅在点击“查看本地原文”后读取；此操作不影响服务器结果页。
          </p>
        </section>
      </div>
    </PanelSection>

    <ConfirmDialog
      :open="revokeConfirmationOpen"
      title="撤销结果链接？"
      description="撤销后此链接不再可读；微信中的旧通知不会消失。"
      confirm-label="撤销链接"
      :busy="actionBusy === 'revoke'"
      @cancel="revokeConfirmationOpen = false"
      @confirm="resultAction('revoke')"
    />
  </div>
</template>

<style scoped>
.view {
  display: grid;
  gap: var(--space-4);
}

.pagination-actions,
.local-body__heading {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-2);
}

.result-summary,
.local-body {
  display: grid;
  gap: var(--space-3);
}

.local-body {
  padding-top: var(--space-3);
  border-top: 1px solid var(--border-default);
}

.result-actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
}

.delivery-body {
  max-height: var(--diagnostics-max-height);
  margin: 0;
  overflow: auto;
  overflow-wrap: anywhere;
  white-space: pre-wrap;
}
</style>
