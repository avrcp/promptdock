import { describe, it, expect } from 'vitest'

import { MOCK_NOW, createMockClock } from './mock-clock'
import { abortableDelay, mockDelay, ZERO_MOCK_DELAY } from './mock-delay'

describe('mock-clock', () => {
  it('returns the initial fixed now', () => {
    const clock = createMockClock()
    expect(clock.now()).toBe(MOCK_NOW)
  })

  it('setNow overrides the current time', () => {
    const clock = createMockClock()
    clock.setNow(123)
    expect(clock.now()).toBe(123)
  })

  it('advance adds to the current time', () => {
    const clock = createMockClock()
    clock.advance(1_000)
    expect(clock.now()).toBe(MOCK_NOW + 1_000)
  })

  it('reset returns to the initial time', () => {
    const clock = createMockClock()
    clock.advance(5_000)
    clock.reset()
    expect(clock.now()).toBe(MOCK_NOW)
  })

  it('supports a custom initial value', () => {
    const clock = createMockClock(99)
    expect(clock.now()).toBe(99)
  })
})

describe('abortableDelay', () => {
  it('resolves after at least the requested time', async () => {
    const start = Date.now()
    await abortableDelay(20)
    expect(Date.now() - start).toBeGreaterThanOrEqual(15)
  })

  it('rejects synchronously if signal is already aborted', async () => {
    const controller = new AbortController()
    controller.abort()
    await expect(abortableDelay(100, controller.signal)).rejects.toThrow(/Aborted/)
  })

  it('rejects when aborted during the delay', async () => {
    const controller = new AbortController()
    const promise = abortableDelay(200, controller.signal)
    controller.abort()
    await expect(promise).rejects.toThrow(/Aborted/)
  })

  it('resolves immediately for 0ms', async () => {
    await expect(abortableDelay(0)).resolves.toBeUndefined()
  })
})

describe('mockDelay', () => {
  it('ZERO_MOCK_DELAY resolves immediately', async () => {
    await expect(mockDelay(ZERO_MOCK_DELAY)).resolves.toBeUndefined()
  })

  it('honors deterministic source for test injection', async () => {
    const start = Date.now()
    await mockDelay({ minMs: 30, maxMs: 30 }, () => 0.5)
    expect(Date.now() - start).toBeGreaterThanOrEqual(25)
  })
})
