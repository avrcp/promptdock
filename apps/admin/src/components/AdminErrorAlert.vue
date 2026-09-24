<script setup lang="ts">
import { computed } from 'vue'
import { useRouter } from 'vue-router'

import InlineAlert from './InlineAlert.vue'
import AppButton from './AppButton.vue'
import CopyValue from './CopyValue.vue'
import type { AdminError } from '@/contracts/error'
import { presentAdminError } from '@/presentation/admin-error-presentation'

const props = withDefaults(
  defineProps<{
    error: AdminError | null
    testid?: string
  }>(),
  { testid: undefined },
)

const emit = defineEmits<{ (e: 'retry'): void }>()

const router = useRouter()

const presentation = computed(() => (props.error ? presentAdminError(props.error) : null))

function onAction(): void {
  const action = presentation.value?.action
  if (!action) return
  if (action.kind === 'retry') {
    emit('retry')
  } else if (action.kind === 'navigate') {
    void router.push(action.to)
  }
}
</script>

<template>
  <div v-if="presentation" class="admin-error-alert" :data-testid="testid">
    <InlineAlert
      :tone="presentation.retryable ? 'warning' : 'danger'"
      :title="presentation.title"
      :description="presentation.description"
      :code="error?.code ?? ''"
    />
    <div
      v-if="presentation.action.kind === 'retry' || presentation.action.kind === 'navigate'"
      class="admin-error-alert__action"
    >
      <AppButton
        size="sm"
        variant="secondary"
        :data-testid="testid ? `${testid}-action` : undefined"
        @click="onAction"
      >
        {{ presentation.action.label }}
      </AppButton>
    </div>
    <div
      v-else-if="presentation.action.kind === 'copy' && presentation.requestId"
      class="admin-error-alert__action"
    >
      <CopyValue :value="presentation.requestId" :label="presentation.action.label" />
    </div>
  </div>
</template>

<style scoped>
.admin-error-alert__action {
  margin-top: var(--pd-space-8);
}
</style>
