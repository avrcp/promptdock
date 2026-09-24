import { describe, expect, it } from 'vitest'

import { presentAdminError } from './admin-error-presentation'
import type { AdminError } from '@/contracts/error'

function err(overrides: Partial<AdminError> = {}): AdminError {
  return {
    code: 'INTERNAL',
    message: 'raw message',
    retryable: true,
    requestId: 'req-123',
    ...overrides,
  }
}

describe('presentAdminError', () => {
  it('RELAY_UNAVAILABLE offers a retry action', () => {
    const p = presentAdminError(err({ code: 'RELAY_UNAVAILABLE' }))
    expect(p.title).toBe('无法连接 Relay')
    expect(p.action.kind).toBe('retry')
    expect(p.retryable).toBe(true)
  })

  it('WECHAT_RECONNECT_REQUIRED navigates to /wechat', () => {
    const p = presentAdminError(err({ code: 'WECHAT_RECONNECT_REQUIRED' }))
    expect(p.action.kind).toBe('navigate')
    if (p.action.kind === 'navigate') {
      expect(p.action.to).toBe('/wechat')
    }
  })

  it('DEVICE_REVOKED offers a navigation to devices', () => {
    const p = presentAdminError(err({ code: 'DEVICE_REVOKED' }))
    expect(p.title).toBe('设备已撤销')
    if (p.action.kind === 'navigate') {
      expect(p.action.to).toBe('/devices')
    }
  })

  it('OUTBOX_BACKLOG points operators to the queue', () => {
    const p = presentAdminError(err({ code: 'OUTBOX_BACKLOG' }))
    if (p.action.kind === 'navigate') {
      expect(p.action.to).toBe('/queue')
    } else {
      throw new Error('expected navigate action')
    }
  })

  it('INTERNAL exposes a copy action when requestId is present', () => {
    const p = presentAdminError(err({ code: 'INTERNAL', requestId: 'req-abc' }))
    expect(p.action.kind).toBe('copy')
    if (p.action.kind === 'copy') {
      expect(p.action.value).toBe('req-abc')
    }
    expect(p.requestId).toBe('req-abc')
  })

  it('INTERNAL without requestId falls back to retry', () => {
    const p = presentAdminError(err({ code: 'INTERNAL', requestId: null, retryable: true }))
    expect(p.action.kind).toBe('retry')
  })

  it('INVALID_INPUT has no recovery action', () => {
    const p = presentAdminError(err({ code: 'INVALID_INPUT' }))
    expect(p.title).toBe('输入内容不符合要求')
    expect(p.action.kind).toBe('none')
  })

  it.each(['CURSOR_INVALID', 'CURSOR_EXPIRED'] as const)(
    '%s describes returning to the first page',
    (code) => {
      const p = presentAdminError(err({ code }))
      expect(p.title).toBe('列表已更新')
      expect(p.description).toContain('第一页')
      expect(p.action.kind).toBe('retry')
      expect(p.retryable).toBe(true)
    },
  )

  it('preserves the request id for downstream copy', () => {
    const p = presentAdminError(err({ requestId: 'req-xyz' }))
    expect(p.requestId).toBe('req-xyz')
  })
})
