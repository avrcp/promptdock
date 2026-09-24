import { describe, it, expect } from 'vitest'

import { createMockClock } from './mock-clock'
import { MockAdminReadRepository } from './mock-admin-repository'
import { AdminRepositoryError } from '@/contracts/error'

function makeRepo(
  overrides: Partial<{ scenario: 'healthy' | 'relay-unavailable' | 'partial-failure' }> = {},
) {
  const clock = createMockClock()
  const id = overrides.scenario ?? 'healthy'
  const repo = new MockAdminReadRepository({
    clock,
    initialScenario: id,
    getScenarioId: () => id,
  })
  return { repo, clock }
}

describe('MockAdminReadRepository', () => {
  it('returns overview in healthy scenario', async () => {
    const { repo } = makeRepo({ scenario: 'healthy' })
    const overview = await repo.getOverview()
    expect(overview.relay.health).toBe('healthy')
    expect(overview.wechat.state).toBe('ready')
  })

  it('throws AdminRepositoryError when overview is marked failed in scenario', async () => {
    const { repo } = makeRepo({ scenario: 'relay-unavailable' })
    await expect(repo.getOverview()).rejects.toBeInstanceOf(AdminRepositoryError)
  })

  it('listDevices paginates and filters by state', async () => {
    const { repo } = makeRepo()
    const page = await repo.listDevices({ state: 'online', limit: 10 })
    expect(page.items.every((d) => d.state === 'online')).toBe(true)
    expect(page.total).toBe(2)
  })

  it('listDevices supports search by name and id', async () => {
    const { repo } = makeRepo()
    const page = await repo.listDevices({ search: '主控', limit: 10 })
    expect(page.items.length).toBe(1)
    expect(page.items[0]?.name).toContain('主控')
  })

  it('getDevice throws DEVICE_NOT_FOUND for unknown id', async () => {
    const { repo } = makeRepo()
    await expect(repo.getDevice('mock-device-does-not-exist')).rejects.toBeInstanceOf(
      AdminRepositoryError,
    )
  })

  it('getWechatStatus reflects current scenario', async () => {
    const { repo } = makeRepo()
    const status = await repo.getWechatStatus()
    expect(status.state).toBe('ready')
  })

  it('listDeliveries returns synthetic deliveries', async () => {
    const { repo } = makeRepo()
    const page = await repo.listDeliveries({ limit: 50 })
    expect(Array.isArray(page.items)).toBe(true)
  })

  it('listInboundCommands filters by command', async () => {
    const { repo } = makeRepo()
    const page = await repo.listInboundCommands({ command: 'help', limit: 10 })
    expect(page.items.every((i) => i.command === 'help')).toBe(true)
  })

  it('getSystemSnapshot returns deterministic build metadata', async () => {
    const { repo } = makeRepo()
    const snap = await repo.getSystemSnapshot()
    expect(snap.build.relayVersion).toBe('mock-relay/0.1.0')
  })

  it('aborts the request when the AbortSignal is triggered', async () => {
    const { repo } = makeRepo()
    const controller = new AbortController()
    const promise = repo.getOverview({ signal: controller.signal })
    controller.abort()
    await expect(promise).rejects.toThrow(/Aborted/)
  })
})
