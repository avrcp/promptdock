export interface MockDelayConfig {
  minMs: number
  maxMs: number
}

export const DEFAULT_MOCK_DELAY: MockDelayConfig = { minMs: 250, maxMs: 450 }
export const SLOW_MOCK_DELAY: MockDelayConfig = { minMs: 1500, maxMs: 2500 }
export const ZERO_MOCK_DELAY: MockDelayConfig = { minMs: 0, maxMs: 0 }

function pickDelay(minMs: number, maxMs: number, source: () => number): number {
  if (maxMs <= minMs) {
    return minMs
  }
  const range = maxMs - minMs
  return Math.floor(source() * range) + minMs
}

export function mockDelay(
  config: MockDelayConfig = DEFAULT_MOCK_DELAY,
  source: () => number = Math.random,
): Promise<void> {
  return abortableDelay(pickDelay(config.minMs, config.maxMs, source))
}

export function abortableDelay(ms: number, signal?: AbortSignal): Promise<void> {
  if (ms <= 0) {
    return Promise.resolve()
  }
  return new Promise<void>((resolve, reject) => {
    if (signal?.aborted) {
      reject(new DOMException('Aborted', 'AbortError'))
      return
    }
    const timer = setTimeout(() => {
      cleanup()
      resolve()
    }, ms)
    const onAbort = (): void => {
      clearTimeout(timer)
      cleanup()
      reject(new DOMException('Aborted', 'AbortError'))
    }
    const cleanup = (): void => {
      signal?.removeEventListener('abort', onAbort)
    }
    signal?.addEventListener('abort', onAbort, { once: true })
  })
}
