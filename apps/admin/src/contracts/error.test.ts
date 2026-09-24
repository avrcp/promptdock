import { describe, expect, it } from 'vitest'

import { AdminRepositoryError, toAdminError, type AdminError } from './error'

const fallback: AdminError = {
  code: 'INTERNAL',
  message: 'fallback',
  retryable: true,
  requestId: null,
}

const wrappedError: AdminError = {
  code: 'RATE_LIMITED',
  message: 'slow down',
  retryable: false,
  requestId: 'req-456',
}

describe('toAdminError', () => {
  it('unwraps the production repository wrapper without losing contract fields', () => {
    expect(toAdminError(new AdminRepositoryError(wrappedError), fallback)).toEqual(wrappedError)
  })

  it('accepts an already normalized AdminError', () => {
    expect(toAdminError(wrappedError, fallback)).toEqual(wrappedError)
  })

  it('uses the supplied fallback for unknown and aborted errors', () => {
    expect(toAdminError(new Error('unexpected'), fallback)).toEqual(fallback)
    expect(toAdminError(new DOMException('cancelled', 'AbortError'), fallback)).toEqual(fallback)
  })
})
