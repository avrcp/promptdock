import { describe, expect, it } from 'vitest'

import type { AdminError } from '@/contracts/error'
import { useCursorPager } from './useCursorPager'

const expiredCursor: AdminError = {
  code: 'CURSOR_EXPIRED',
  message: 'cursor expired',
  retryable: true,
  requestId: 'req-cursor',
}

describe('useCursorPager', () => {
  it('keeps cursors opaque while navigating forward and backward', () => {
    const pager = useCursorPager()

    expect(pager.next('opaque-2')).toBe(true)
    expect(pager.next('opaque-3')).toBe(true)
    expect(pager.page.value).toBe(3)
    expect(pager.previous()).toBe(true)
    expect(pager.cursor.value).toBe('opaque-2')
    expect(pager.page.value).toBe(2)
  })

  it('clears the entire history when filters change', () => {
    const pager = useCursorPager()
    pager.next('opaque-2')
    pager.next('opaque-3')

    pager.reset()

    expect(pager.cursor.value).toBeNull()
    expect(pager.history.value).toEqual([])
    expect(pager.page.value).toBe(1)
    expect(pager.hasPrevious.value).toBe(false)
  })

  it('resets a later page once when Relay rejects its cursor', () => {
    const pager = useCursorPager()
    pager.next('opaque-2')

    expect(pager.recoverFromCursorError(expiredCursor)).toBe(true)
    expect(pager.cursor.value).toBeNull()
    expect(pager.history.value).toEqual([])
    expect(pager.recoverFromCursorError(expiredCursor)).toBe(false)
    expect(pager.recoverFromCursorError({ ...expiredCursor, code: 'INTERNAL' })).toBe(false)
  })
})
