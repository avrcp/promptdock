import { describe, it, expect } from 'vitest'

import { createMockClock } from './mock-clock'
import { MockAdminCommandRepository } from './mock-command-repository'
import { AdminRepositoryError } from '@/contracts/error'

function makeRepo() {
  const clock = createMockClock()
  return new MockAdminCommandRepository({
    clock,
    getScenarioId: () => 'healthy',
  })
}

describe('MockAdminCommandRepository', () => {
  it('createDevice returns a synthetic one-time token', async () => {
    const repo = makeRepo()
    const receipt = await repo.createDevice({
      name: '新建设备',
      scopes: ['gateway:connect'],
    })
    expect(receipt.action).toBe('create')
    expect(receipt.oneTimeToken.startsWith('mock-pdv2.not-a-real-secret.')).toBe(true)
  })

  it('rotateDevice records rotation time and returns a new token', async () => {
    const repo = makeRepo()
    const receipt = await repo.rotateDevice({ deviceId: 'mock-device-alpha' })
    expect(receipt.action).toBe('rotate')
    expect(receipt.oneTimeToken.startsWith('mock-pdv2.not-a-real-secret.')).toBe(true)
  })

  it('rotateDevice fails for unknown id', async () => {
    const repo = makeRepo()
    await expect(repo.rotateDevice({ deviceId: 'mock-device-x' })).rejects.toBeInstanceOf(
      AdminRepositoryError,
    )
  })

  it('setDeviceEnabled toggles enable / disable', async () => {
    const repo = makeRepo()
    const receipt = await repo.setDeviceEnabled({
      deviceId: 'mock-device-alpha',
      enabled: false,
    })
    expect(receipt.action).toBe('device.disable')
  })

  it('revokeDevice marks device revoked and blocks subsequent setDeviceEnabled', async () => {
    const repo = makeRepo()
    await repo.revokeDevice({ deviceId: 'mock-device-beta' })
    await expect(
      repo.setDeviceEnabled({ deviceId: 'mock-device-beta', enabled: true }),
    ).rejects.toBeInstanceOf(AdminRepositoryError)
  })

  it('startWechatLogin produces a real-shape snapshot and can be cancelled', async () => {
    const repo = makeRepo()
    const session = await repo.startWechatLogin({ forceFresh: false })
    expect(session.state).toBe('waiting_scan')
    expect(session.qrContent).toContain('https://')
    expect(session.canSubmitVerifyCode).toBe(false)
    const cancel = await repo.cancelWechatLogin(session.loginId)
    expect(cancel.state).toBe('cancelled')
  })

  it('cancelWechatLogin rejects an unknown session id', async () => {
    const repo = makeRepo()
    await expect(repo.cancelWechatLogin('mock-login-does-not-exist')).rejects.toBeInstanceOf(
      AdminRepositoryError,
    )
  })

  it('sendWechatTest returns accepted_by_relay by default', async () => {
    const repo = makeRepo()
    const receipt = await repo.sendWechatTest()
    expect(receipt.state).toBe('accepted_by_relay')
  })

  it('disconnectWechat returns an action receipt', async () => {
    const repo = makeRepo()
    const receipt = await repo.disconnectWechat({ reason: 'mock test' })
    expect(receipt.action).toBe('wechat.disconnect')
  })

  it('runRetention returns a maintenance receipt', async () => {
    const repo = makeRepo()
    const receipt = await repo.runRetention()
    expect(receipt.action).toBe('maintenance.retention')
  })
})
