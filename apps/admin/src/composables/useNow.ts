import { onBeforeUnmount, ref, type Ref } from 'vue'

const TICK_MS = 30_000

// A single app-wide clock.  Every TimeAgo (and any freshness label) subscribes
// to the same ref, so a 50-row queue table spins up one 30s timer instead of
// fifty.  The interval is reference-counted so it stops once the last consumer
// unmounts.
const sharedNow = ref(Date.now())
let timer: ReturnType<typeof setInterval> | null = null
let consumers = 0

function startClock(): void {
  if (timer !== null) return
  timer = setInterval(() => {
    sharedNow.value = Date.now()
  }, TICK_MS)
}

function stopClock(): void {
  if (timer === null) return
  clearInterval(timer)
  timer = null
}

export function useNow(): { now: Ref<number> } {
  consumers += 1
  startClock()
  onBeforeUnmount(() => {
    consumers -= 1
    if (consumers <= 0) {
      consumers = 0
      stopClock()
    }
  })
  return { now: sharedNow }
}
