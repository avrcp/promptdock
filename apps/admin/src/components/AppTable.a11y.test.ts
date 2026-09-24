import { describe, it, expect } from 'vitest'
import { mount } from '@vue/test-utils'

import AppTable from './AppTable.vue'

interface Row {
  id: string
  name: string
}

const columns = [
  { key: 'name', label: '名称' },
  { key: 'actions', label: '操作' },
]

const rows: Row[] = [
  { id: '1', name: '设备一' },
  { id: '2', name: '设备二' },
]

function mountTable(propsOverride: Record<string, unknown> = {}): ReturnType<typeof mount> {
  return mount(AppTable<Row>, {
    props: {
      columns,
      rows,
      rowKey: (r: Row) => r.id,
      ariaLabel: '设备列表（2 条）',
      ...propsOverride,
    },
    slots: {
      'cell-actions': '<button type="button" class="inner-btn">操作</button>',
    },
    attachTo: document.body,
  })
}

describe('AppTable accessibility', () => {
  it('exposes column headers with scope=col', () => {
    const wrapper = mountTable()
    const headers = wrapper.findAll('th')
    expect(headers.length).toBe(2)
    for (const th of headers) {
      expect(th.attributes('scope')).toBe('col')
    }
    wrapper.unmount()
  })

  it('does not make rows keyboard focusable by default', () => {
    // Spec: rows are not tab stops unless a page explicitly opts in.  This
    // prevents a 50-row table from adding 50 Tab stops on top of the
    // action buttons inside each row.
    const wrapper = mountTable()
    const row = wrapper.find('.app-table__row')
    expect(row.attributes('tabindex')).toBeUndefined()
    expect(row.attributes('aria-label')).toBeUndefined()
    wrapper.unmount()
  })

  it('opts in to interactive rows with an accessible name when interactiveRows=true', () => {
    const wrapper = mountTable({ interactiveRows: true })
    const row = wrapper.find('.app-table__row')
    expect(row.attributes('tabindex')).toBe('0')
    expect(row.attributes('aria-label')).toBe('查看 设备一 详情')
    wrapper.unmount()
  })

  it('emits row-click on Enter key when interactiveRows=true', async () => {
    const wrapper = mountTable({ interactiveRows: true })
    const row = wrapper.find('.app-table__row')
    await row.trigger('keydown', { key: 'Enter' })
    expect(wrapper.emitted('row-click')?.[0]?.[0]).toMatchObject({ id: '1' })
    wrapper.unmount()
  })

  it('emits row-click on Space key when interactiveRows=true', async () => {
    const wrapper = mountTable({ interactiveRows: true })
    const row = wrapper.find('.app-table__row')
    await row.trigger('keydown', { key: ' ' })
    expect(wrapper.emitted('row-click')?.[0]?.[0]).toMatchObject({ id: '1' })
    wrapper.unmount()
  })

  it('does not emit row-click for keys pressed inside a nested button', async () => {
    const wrapper = mountTable({ interactiveRows: true })
    const button = wrapper.find('.inner-btn')
    await button.trigger('keydown', { key: 'Enter' })
    expect(wrapper.emitted('row-click')).toBeUndefined()
    wrapper.unmount()
  })

  it('marks the table loading state with aria-busy', () => {
    const wrapper = mountTable({ loading: true })
    expect(wrapper.find('.app-table').attributes('aria-busy')).toBe('true')
    expect(wrapper.find('.app-table__loading-rail').exists()).toBe(true)
    wrapper.unmount()
  })

  it('shows loading cell instead of empty text during initial load', () => {
    const wrapper = mount(AppTable<Row>, {
      props: { columns, rows: [], rowKey: (r: Row) => r.id, loading: true },
    })
    expect(wrapper.find('.app-table__loading-cell').exists()).toBe(true)
    expect(wrapper.find('.app-table__loading-cell').text()).toBe('正在加载…')
    expect(wrapper.find('.app-table__empty').exists()).toBe(false)
    wrapper.unmount()
  })

  it('keeps existing rows visible during background refresh', () => {
    const wrapper = mountTable({ loading: true })
    expect(wrapper.findAll('.app-table__row')).toHaveLength(2)
    expect(wrapper.find('.app-table__loading-rail').exists()).toBe(true)
    expect(wrapper.find('.app-table__loading-cell').exists()).toBe(false)
    wrapper.unmount()
  })

  it('keeps action buttons accessible when rows are non-interactive', () => {
    // The non-interactive default is the new contract: action cells are
    // reached through their own tabindex, not via a row-level wrapper.
    const wrapper = mount(AppTable<Row>, {
      props: { columns, rows, rowKey: (r: Row) => r.id },
      slots: { 'cell-actions': '<button type="button">查看详情</button>' },
    })
    const row = wrapper.find('.app-table__row')
    expect(row.attributes('tabindex')).toBeUndefined()
    expect(row.attributes('aria-label')).toBeUndefined()
    expect(wrapper.find('button').exists()).toBe(true)
    wrapper.unmount()
  })

  it('applies column align classes to th and td elements', () => {
    const alignedColumns = [
      { key: 'name', label: '名称' },
      { key: 'count', label: '数量', align: 'right' as const },
    ]
    const alignedRows: Row[] = [{ id: '1', name: '设备一' }]
    const wrapper = mount(AppTable<Row>, {
      props: { columns: alignedColumns, rows: alignedRows, rowKey: (r: Row) => r.id },
    })
    const headers = wrapper.findAll('th')
    expect(headers).toHaveLength(2)
    expect(headers[0]!.classes()).toContain('app-table__th--left')
    expect(headers[1]!.classes()).toContain('app-table__th--right')
    const cells = wrapper.findAll('td')
    expect(cells).toHaveLength(2)
    expect(cells[0]!.classes()).toContain('app-table__td--left')
    expect(cells[1]!.classes()).toContain('app-table__td--right')
    wrapper.unmount()
  })
})
