export const MOCK_NOW = 1_787_652_000_000

export interface MockClock {
  now(): number
  setNow(value: number): void
  advance(deltaMs: number): void
  reset(): void
}

export function createMockClock(initial: number = MOCK_NOW): MockClock {
  let current = initial
  return {
    now(): number {
      return current
    },
    setNow(value: number): void {
      current = value
    },
    advance(deltaMs: number): void {
      current = current + deltaMs
    },
    reset(): void {
      current = initial
    },
  }
}
