<script lang="ts">
export interface ColumnDef {
  key: string
  label: string
  width?: string
  align?: 'left' | 'right' | 'center'
}

export interface AppTableProps<T extends { id: string | number }> {
  columns: ColumnDef[]
  rows: T[]
  rowKey: (row: T) => string | number
  caption?: string
  ariaLabel?: string
  emptyText?: string
  loading?: boolean
  interactiveRows?: boolean
}
</script>

<script setup lang="ts" generic="T extends { id: string | number }">
withDefaults(defineProps<AppTableProps<T>>(), {
  caption: '',
  ariaLabel: '数据表',
  emptyText: '暂无数据',
  loading: false,
  // The default is intentionally false: making every <tr> a Tab stop on a
  // 50-row table adds 50 stops before reaching the action buttons, and
  // conflates row focus with explicit per-row actions.  Pages that need
  // row-level interaction must opt in AND provide a single, clearly
  // labelled entry point (typically a "view details" button in a named
  // cell).
  interactiveRows: false,
})

const emit = defineEmits<{
  (e: 'row-click', row: T): void
}>()

function rowLabel(row: T): string {
  const name = (row as Record<string, unknown>)['name']
  if (typeof name === 'string' && name.length > 0) return `查看 ${name} 详情`
  return '查看详情'
}

function cellValue(row: T, key: string): unknown {
  return (row as Record<string, unknown>)[key]
}

function onRowKeydown(event: KeyboardEvent, row: T): void {
  if (event.target !== event.currentTarget) return
  if (event.key === 'Enter' || event.key === ' ') {
    event.preventDefault()
    emit('row-click', row)
  }
}
</script>

<template>
  <div class="app-table" :aria-busy="loading || undefined" data-testid="app-table">
    <span v-if="loading" class="app-table__loading-rail" aria-hidden="true" />
    <table :aria-label="ariaLabel">
      <caption v-if="caption" class="app-table__caption">{{ caption }}</caption>
      <thead>
        <tr>
          <th
            v-for="col in columns"
            :key="col.key"
            :style="col.width ? { width: col.width } : undefined"
            :class="['app-table__th', `app-table__th--${col.align ?? 'left'}`]"
            scope="col"
          >
            {{ col.label }}
          </th>
        </tr>
      </thead>
      <tbody v-if="loading && rows.length === 0">
        <tr>
          <td :colspan="columns.length" class="app-table__loading-cell">正在加载…</td>
        </tr>
      </tbody>
      <tbody v-else-if="rows.length > 0">
        <tr
          v-for="row in rows"
          :key="rowKey(row)"
          :class="['app-table__row', { 'app-table__row--interactive': interactiveRows }]"
          :tabindex="interactiveRows ? 0 : undefined"
          :aria-label="interactiveRows ? rowLabel(row) : undefined"
          @click="interactiveRows && emit('row-click', row)"
          @keydown="interactiveRows && onRowKeydown($event, row)"
        >
          <td
            v-for="col in columns"
            :key="col.key"
            :class="['app-table__td', `app-table__td--${col.align ?? 'left'}`]"
          >
            <slot v-if="$slots[`cell-${col.key}`]" :name="`cell-${col.key}`" :row="row" />
            <span v-else>{{ cellValue(row, col.key) }}</span>
          </td>
        </tr>
      </tbody>
      <tbody v-else>
        <tr>
          <td :colspan="columns.length" class="app-table__empty">
            {{ emptyText }}
          </td>
        </tr>
      </tbody>
    </table>
  </div>
</template>

<style scoped>
.app-table {
  position: relative;
  width: 100%;
  overflow-x: auto;
  background: var(--pd-container-panel-bg);
  border: 1px solid var(--pd-border-separator);
  border-radius: var(--pd-radius-md);
}

.app-table > table {
  min-width: 560px;
}

table {
  width: 100%;
  border-collapse: collapse;
  table-layout: auto;
}

.app-table__caption {
  text-align: start;
  padding: var(--pd-space-8) var(--pd-space-12);
  font-size: var(--pd-font-size-13);
  line-height: var(--pd-line-height-ui);
  color: var(--pd-text-muted);
  background: var(--pd-container-header-bg);
  border-bottom: 1px solid var(--pd-border-separator);
}

thead {
  background: var(--pd-container-header-bg);
}

.app-table__th {
  height: var(--pd-table-header-height);
  padding: 0 var(--pd-space-12);
  font-size: var(--pd-font-size-12);
  font-weight: var(--pd-font-weight-medium);
  line-height: var(--pd-line-height-metadata);
  color: var(--pd-text-muted);
  text-align: start;
  border-bottom: 1px solid var(--pd-border-separator);
  white-space: nowrap;
}

.app-table__th--right {
  text-align: end;
}

.app-table__th--center {
  text-align: center;
}

.app-table__row {
  height: var(--pd-table-row-height);
  transition: background var(--pd-transition-fast);
}

.app-table__row:hover {
  background: var(--pd-control-bg-hovered);
}

.app-table__row--interactive {
  cursor: pointer;
}

.app-table__row--interactive:focus-visible {
  outline: 2px solid var(--pd-border-focus);
  outline-offset: -2px;
}

.app-table__td {
  padding: 0 var(--pd-space-12);
  font-size: var(--pd-font-size-13);
  line-height: var(--pd-line-height-ui);
  color: var(--pd-text-default);
  border-bottom: 1px solid var(--pd-border-separator);
  vertical-align: middle;
}

.app-table__row:last-child .app-table__td {
  border-bottom: none;
}

.app-table__td--right {
  text-align: end;
}

.app-table__td--center {
  text-align: center;
}

.app-table__empty {
  padding: var(--pd-space-32);
  text-align: center;
  color: var(--pd-text-subtle);
  font-size: var(--pd-font-size-13);
  line-height: var(--pd-line-height-ui);
}

.app-table__loading-rail {
  display: block;
  width: 100%;
  height: var(--pd-space-2);
  background: var(--pd-primary);
}

.app-table__loading-cell {
  padding: var(--pd-space-32);
  text-align: center;
  color: var(--pd-text-subtle);
  font-size: var(--pd-font-size-13);
  line-height: var(--pd-line-height-ui);
}
</style>
