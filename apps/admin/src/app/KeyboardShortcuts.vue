<script setup lang="ts">
import { useRouter } from 'vue-router'

import AppDialog from '@/components/AppDialog.vue'
import { useKeyboardShortcuts, SHORTCUT_HELP } from '@/composables/useKeyboardShortcuts'

const router = useRouter()
const { showHelp, closeHelp } = useKeyboardShortcuts(router)
</script>

<template>
  <AppDialog
    :open="showHelp"
    title="键盘快捷键"
    description="以下快捷键仅在输入框、文本域、下拉框或中文输入法之外生效。"
    primary-label="关闭"
    @update:open="(v: boolean) => v || closeHelp()"
    @primary="closeHelp"
  >
    <dl class="kshortcuts">
      <div v-for="item in SHORTCUT_HELP" :key="item.keys" class="kshortcuts__row">
        <dt class="kshortcuts__keys">
          <kbd v-for="(key, i) in item.keys.split(' ')" :key="i" class="kshortcuts__key">
            {{ key }}
          </kbd>
        </dt>
        <dd class="kshortcuts__label">{{ item.label }}</dd>
      </div>
    </dl>
  </AppDialog>
</template>

<style scoped>
.kshortcuts {
  display: flex;
  flex-direction: column;
  gap: var(--pd-space-8);
  margin: 0;
}

.kshortcuts__row {
  display: flex;
  align-items: center;
  gap: var(--pd-space-12);
}

.kshortcuts__keys {
  flex-shrink: 0;
  display: inline-flex;
  gap: var(--pd-space-4);
  margin: 0;
}

.kshortcuts__key {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: var(--pd-keycap-height);
  height: var(--pd-keycap-height);
  padding: 0 var(--pd-space-4);
  border-radius: var(--pd-radius-sm);
  border: 1px solid var(--pd-border-emphasis);
  background: var(--pd-container-panel-bg);
  color: var(--pd-text-default);
  font-family: var(--pd-font-code);
  font-size: var(--pd-font-size-12);
}

.kshortcuts__label {
  margin: 0;
  color: var(--pd-text-muted);
  font-size: var(--pd-font-size-13);
}
</style>
