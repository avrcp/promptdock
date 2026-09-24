import { flushPromises } from '@vue/test-utils'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createActivityController } from './activity'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }))

const mockedInvoke = vi.mocked(invoke)
const mockedListen = vi.mocked(listen)
const item = (runKey = 'run-1', revision = 2) => ({
  runKey,
  workspaceLabel: 'workspace',
  displayTitle: '安全标题',
  activityRevision: revision,
  phase: 'settling' as const,
  startedAt: null,
  lastObservedAt: 1_788_970_000,
  attention: {
    revision,
    acknowledgedRevision: revision - 1,
    label: '等待处理',
    historical: false,
    observationExpiresAt: null,
  },
  result: null,
  delivery: null,
})
const page = (items = [item()]) => ({
  items,
  nextCursor: null,
  counts: {
    attention: items.length,
    started: 0,
    results: 0,
    deliveryIssues: 0,
    recent: items.length,
  },
})

beforeEach(() => {
  mockedInvoke.mockReset()
  mockedListen.mockReset()
  mockedListen.mockResolvedValue(() => undefined)
  mockedInvoke.mockImplementation(async (command) => {
    if (command === 'desktop_activity_page') return page() as never
    if (command === 'desktop_activity_detail')
      return { item: item(), events: [], deliveries: [] } as never
    return null as never
  })
})

describe('activity controller', () => {
  it('rejects an unknown page DTO without replacing the current snapshot', async () => {
    const controller = createActivityController()
    await controller.refresh()
    mockedInvoke.mockResolvedValueOnce({ items: [{ runKey: 'unsafe' }] } as never)
    await controller.refresh()
    expect(controller.items.value).toEqual([item()])
    expect(controller.error.value).toContain('回执无法确认')
    controller.dispose()
  })

  it('drops an old response when a newer snapshot wins', async () => {
    let resolveOld!: (value: unknown) => void
    mockedInvoke.mockImplementation(async (command) => {
      if (command !== 'desktop_activity_page') return null as never
      return new Promise((resolve) => {
        resolveOld = resolve
      }) as never
    })
    const controller = createActivityController()
    const first = controller.refresh()
    await flushPromises()
    // A dirty event during a single-flight read requires a fresh snapshot.
    const second = controller.refresh()
    resolveOld(page([item('old')]))
    await flushPromises()
    // Second cycle has started; answer it with the newer record.
    resolveOld(page([item('new')]))
    await Promise.all([first, second])
    expect(controller.items.value.map((entry) => entry.runKey)).toEqual(['new'])
    controller.dispose()
  })

  it('uses click-time revisions for acknowledgement and refreshes after the receipt', async () => {
    const controller = createActivityController()
    await controller.refresh()
    await controller.acknowledge(controller.items.value[0]!)
    expect(mockedInvoke).toHaveBeenCalledWith('desktop_attention_ack', {
      runKey: 'run-1',
      observedRevision: 2,
    })
    expect(
      mockedInvoke.mock.calls.filter(([command]) => command === 'desktop_activity_page').length,
    ).toBe(2)
    controller.dispose()
  })

  it('maps the UI delivery-issues filter to the Rust wire enum', async () => {
    const controller = createActivityController()
    await controller.setFilter('deliveryIssues')
    expect(mockedInvoke).toHaveBeenCalledWith('desktop_activity_page', {
      filter: 'delivery_issues',
      cursor: null,
      limit: 20,
    })
    controller.dispose()
  })

  it('does not apply a late detail after disposal', async () => {
    let resolveDetail!: (value: unknown) => void
    mockedInvoke.mockImplementation(async (command) => {
      if (command === 'desktop_activity_detail')
        return new Promise((resolve) => {
          resolveDetail = resolve
        }) as never
      if (command === 'desktop_activity_page') return page() as never
      return null as never
    })
    const controller = createActivityController()
    await controller.refresh()
    const loading = controller.openDetail('run-1')
    controller.dispose()
    resolveDetail({ item: item(), events: [], deliveries: [] })
    await loading
    expect(controller.selected.value).toBeNull()
  })

  it('keeps the selected run when an older detail response returns late', async () => {
    let resolveFirst!: (value: unknown) => void
    let resolveSecond!: (value: unknown) => void
    mockedInvoke.mockImplementation(async (command, args) => {
      if (command === 'desktop_activity_page') return page([item('first'), item('second')]) as never
      if (command === 'desktop_activity_detail')
        return new Promise((resolve) => {
          if ((args as { runKey: string }).runKey === 'first') resolveFirst = resolve
          else resolveSecond = resolve
        }) as never
      return null as never
    })
    const controller = createActivityController()
    await controller.refresh()
    const first = controller.openDetail('first')
    const second = controller.openDetail('second')
    resolveSecond({ item: item('second'), events: [], deliveries: [] })
    await second
    resolveFirst({ item: item('first'), events: [], deliveries: [] })
    await first
    expect(controller.selected.value?.item.runKey).toBe('second')
    controller.dispose()
  })

  it('does not move the card that contains the focused action during a head refresh', async () => {
    let calls = 0
    mockedInvoke.mockImplementation(async (command) => {
      if (command !== 'desktop_activity_page') return null as never
      calls += 1
      return (
        calls === 1 ? page([item('first'), item('second')]) : page([item('second'), item('first')])
      ) as never
    })
    const controller = createActivityController()
    await controller.refresh()
    const fixture = document.createElement('article')
    fixture.dataset.runKey = 'second'
    fixture.innerHTML = '<button type="button">确认</button>'
    document.body.append(fixture)
    fixture.querySelector('button')!.focus()
    await controller.refresh()
    expect(controller.items.value.map((entry) => entry.runKey)).toEqual(['first', 'second'])
    fixture.remove()
    controller.dispose()
  })

  it('recovers with one fresh snapshot when focus returns', async () => {
    const controller = createActivityController()
    await controller.start()
    mockedInvoke.mockClear()
    window.dispatchEvent(new Event('focus'))
    await flushPromises()
    expect(mockedInvoke).toHaveBeenCalledWith('desktop_activity_page', {
      filter: 'attention',
      cursor: null,
      limit: 20,
    })
    controller.dispose()
  })

  it('refreshes a visible activity snapshot on the bounded timer and clears it on dispose', async () => {
    vi.useFakeTimers()
    const controller = createActivityController()
    await controller.start()
    mockedInvoke.mockClear()
    await vi.advanceTimersByTimeAsync(30_000)
    expect(mockedInvoke).toHaveBeenCalledWith('desktop_activity_page', expect.any(Object))
    controller.dispose()
    expect(vi.getTimerCount()).toBe(0)
    vi.useRealTimers()
  })
})
