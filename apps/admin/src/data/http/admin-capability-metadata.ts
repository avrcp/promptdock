import { adminMetaSchema, type AdminMeta } from '@promptdock/relay-admin-api-generated'
import { HttpClient } from './http-client'

export const ADMIN_CAPABILITY_TTL_MS = 60_000

export interface CapabilityMetadataRequest {
  force?: boolean
}

export interface AdminCapabilityMetadataOptions {
  client: HttpClient
  ttlMs?: number
  now?: () => number
}

/**
 * One authoritative capability receipt per production repository bundle.
 *
 * The store deliberately owns caching and invalidation rather than letting read
 * and command repositories each retain a potentially divergent copy. A forced
 * refresh is latest-wins; ordinary callers share the same in-flight request.
 */
export class AdminCapabilityMetadata {
  private readonly client: HttpClient
  private readonly ttlMs: number
  private readonly now: () => number
  private readonly listeners = new Set<() => void>()
  private meta: AdminMeta | null = null
  private lastSuccessAt: number | null = null
  private active: {
    epoch: number
    controller: AbortController
    promise: Promise<AdminMeta>
  } | null = null
  private epoch = 0

  constructor(options: AdminCapabilityMetadataOptions) {
    this.client = options.client
    this.ttlMs = options.ttlMs ?? ADMIN_CAPABILITY_TTL_MS
    this.now = options.now ?? Date.now
    if (!Number.isSafeInteger(this.ttlMs) || this.ttlMs <= 0) {
      throw new RangeError('capability metadata TTL must be a positive safe integer')
    }
    this.client.onAuthorizationFailure(({ path }) => {
      // A rejected /meta request cannot establish that capabilities changed and
      // must not recursively cause its own retry loop.
      if (path !== '/meta') this.invalidate()
    })
  }

  get(request: CapabilityMetadataRequest = {}): Promise<AdminMeta> {
    if (!request.force && this.meta && !this.isStale()) {
      return Promise.resolve(this.meta)
    }
    if (this.active && !request.force) {
      return this.active.promise
    }
    if (this.active) {
      this.active.controller.abort()
    }

    const controller = new AbortController()
    const epoch = ++this.epoch
    const promise = this.client
      .request({
        path: '/meta',
        signal: controller.signal,
        schema: adminMetaSchema,
        purpose: 'read',
      })
      .then((response) => {
        if (epoch !== this.epoch) {
          throw new DOMException('Superseded capability metadata request', 'AbortError')
        }
        this.meta = response.data
        this.lastSuccessAt = this.now()
        return response.data
      })
      .finally(() => {
        if (this.active?.epoch === epoch) this.active = null
      })
    this.active = { epoch, controller, promise }
    return promise
  }

  invalidate(): void {
    this.lastSuccessAt = null
    for (const listener of this.listeners) listener()
  }

  isStale(now: number = this.now()): boolean {
    return (
      this.meta === null || this.lastSuccessAt === null || now - this.lastSuccessAt >= this.ttlMs
    )
  }

  getLastSuccessAt(): number | null {
    return this.lastSuccessAt
  }

  subscribeInvalidation(listener: () => void): () => void {
    this.listeners.add(listener)
    return () => this.listeners.delete(listener)
  }
}
