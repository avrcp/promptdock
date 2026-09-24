import { z } from 'zod'

import { withQuery, type QueryParameters } from './query'

export const ADMIN_API_BASE_PATH = '/admin/api/v2'
export const DEFAULT_MAX_RESPONSE_BYTES = 512 * 1024
export const DEFAULT_HTTP_DEADLINES = {
  read: 15_000,
  mutation: 30_000,
  'login-start': 15_000,
  'login-verify': 15_000,
  'login-poll': 10_000,
} as const

type FetchImplementation = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>

export type HttpMethod = 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE'
export type HttpRequestPurpose = 'read' | 'mutation' | 'login-start' | 'login-verify' | 'login-poll'

export interface HttpAuthorizationFailure {
  path: string
  status: 401 | 403
}

export interface HttpClientRequest<T = unknown> {
  path: string
  method?: HttpMethod
  query?: QueryParameters
  body?: unknown
  signal?: AbortSignal
  /** A strict schema is supplied by the wire-contract adapter, never inferred here. */
  schema?: z.ZodType<T>
  headers?: HeadersInit
  /** Selects a deliberate default deadline; callers can override via timeoutMs in tests/hosts. */
  purpose?: HttpRequestPurpose
  timeoutMs?: number
}

export interface HttpJsonResponse<T> {
  data: T
  status: number
  headers: Headers
}

export type HttpClientErrorCode =
  | 'NETWORK_ERROR'
  | 'INVALID_CONTENT_TYPE'
  | 'RESPONSE_TOO_LARGE'
  | 'INVALID_JSON'
  | 'HTTP_ERROR'
  | 'REQUEST_SERIALIZATION_ERROR'
  | 'REQUEST_TIMEOUT'
  | 'ADMIN_CONTRACT_MISMATCH'

export class HttpClientError extends Error {
  readonly code: HttpClientErrorCode
  readonly status: number | null
  readonly requestId: string | null
  readonly retryAfterSeconds: number | null
  readonly responseCode: string | null
  readonly retryable: boolean
  /** A bounded schema field path only; never a serialized response value. */
  readonly contractPath: string | null

  constructor(options: {
    code: HttpClientErrorCode
    message: string
    status?: number | null
    requestId?: string | null
    retryAfterSeconds?: number | null
    responseCode?: string | null
    retryable?: boolean
    contractPath?: string | null
  }) {
    super(options.message)
    this.name = 'HttpClientError'
    this.code = options.code
    this.status = options.status ?? null
    this.requestId = options.requestId ?? null
    this.retryAfterSeconds = options.retryAfterSeconds ?? null
    this.responseCode = options.responseCode ?? null
    this.retryable = options.retryable ?? false
    this.contractPath = options.contractPath ?? null
  }
}

export interface HttpClientOptions {
  basePath?: string
  fetch?: FetchImplementation
  maxResponseBytes?: number
  deadlines?: Partial<Record<HttpRequestPurpose, number>>
  setTimeout?: (handler: () => void, timeoutMs: number) => ReturnType<typeof setTimeout>
  clearTimeout?: (timer: ReturnType<typeof setTimeout>) => void
}

export class HttpClient {
  private readonly basePath: string
  private readonly fetchImplementation: FetchImplementation
  private readonly maxResponseBytes: number
  private readonly deadlines: Record<HttpRequestPurpose, number>
  private readonly setTimer: (
    handler: () => void,
    timeoutMs: number,
  ) => ReturnType<typeof setTimeout>
  private readonly clearTimer: (timer: ReturnType<typeof setTimeout>) => void
  private readonly authorizationFailureListeners = new Set<
    (failure: HttpAuthorizationFailure) => void
  >()

  constructor(options: HttpClientOptions = {}) {
    this.basePath = normalizeBasePath(options.basePath ?? ADMIN_API_BASE_PATH)
    this.fetchImplementation = options.fetch ?? globalThis.fetch.bind(globalThis)
    this.maxResponseBytes = options.maxResponseBytes ?? DEFAULT_MAX_RESPONSE_BYTES
    if (!Number.isSafeInteger(this.maxResponseBytes) || this.maxResponseBytes <= 0) {
      throw new RangeError('maxResponseBytes must be a positive safe integer')
    }
    this.deadlines = { ...DEFAULT_HTTP_DEADLINES, ...options.deadlines }
    for (const deadline of Object.values(this.deadlines)) {
      if (!Number.isSafeInteger(deadline) || deadline <= 0) {
        throw new RangeError('HTTP request deadlines must be positive safe integers')
      }
    }
    this.setTimer = options.setTimeout ?? globalThis.setTimeout.bind(globalThis)
    this.clearTimer = options.clearTimeout ?? globalThis.clearTimeout.bind(globalThis)
  }

  onAuthorizationFailure(listener: (failure: HttpAuthorizationFailure) => void): () => void {
    this.authorizationFailureListeners.add(listener)
    return () => this.authorizationFailureListeners.delete(listener)
  }

  async request<T = unknown>(request: HttpClientRequest<T>): Promise<HttpJsonResponse<T>> {
    const method = request.method ?? 'GET'
    validateRelativePath(request.path)
    const path = withQuery(request.path, request.query)
    const url = joinRelativeUrl(this.basePath, path)
    const headers = new Headers(request.headers)
    const isWrite = method !== 'GET'

    let body: string | undefined
    if (request.body !== undefined) {
      try {
        body = JSON.stringify(request.body)
      } catch {
        throw new HttpClientError({
          code: 'REQUEST_SERIALIZATION_ERROR',
          message: '请求内容无法序列化。',
        })
      }
      headers.set('Content-Type', 'application/json')
    }
    if (isWrite) {
      headers.set('X-PromptDock-Admin-Action', '1')
    }

    const deadlineController = new AbortController()
    const timeoutMs = request.timeoutMs ?? this.deadlines[request.purpose ?? defaultPurpose(method)]
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs <= 0) {
      throw new RangeError('HTTP request timeout must be a positive safe integer')
    }
    const combinedSignal = combineAbortSignals(request.signal, deadlineController.signal)
    const signal = combinedSignal.signal
    const timer = this.setTimer(() => deadlineController.abort(), timeoutMs)

    try {
      let response: Response
      try {
        response = await this.fetchImplementation(url, {
          method,
          credentials: 'same-origin',
          cache: 'no-store',
          signal,
          headers,
          ...(body === undefined ? {} : { body }),
        })
      } catch (error) {
        if (deadlineController.signal.aborted && !request.signal?.aborted) {
          throw requestTimeoutError()
        }
        if (isAbortError(error)) {
          throw error
        }
        throw new HttpClientError({
          code: 'NETWORK_ERROR',
          message: '无法连接到 Relay Admin。',
          retryable: true,
        })
      }
      throwIfTimedOut(deadlineController.signal, request.signal)
      const requestId = response.headers.get('X-Request-Id')
      const retryAfterSeconds = parseRetryAfter(response.headers.get('Retry-After'))
      if (
        (response.status === 401 || response.status === 403) &&
        pathWithoutQuery(request.path) !== '/meta'
      ) {
        this.notifyAuthorizationFailure({
          path: pathWithoutQuery(request.path),
          status: response.status,
        })
      }
      if (
        contentLengthExceedsLimit(response.headers.get('Content-Length'), this.maxResponseBytes)
      ) {
        throw new HttpClientError({
          code: 'RESPONSE_TOO_LARGE',
          message: 'Relay Admin 响应过大。',
          status: response.status,
          requestId,
          retryAfterSeconds,
          retryable: false,
        })
      }
      const contentType = response.headers.get('Content-Type')
      if (!isJsonContentType(contentType)) {
        throw new HttpClientError({
          code: 'INVALID_CONTENT_TYPE',
          message: 'Relay Admin 返回了非 JSON 响应。',
          status: response.status,
          requestId,
          retryAfterSeconds,
          retryable: response.status >= 500,
        })
      }

      let text: string
      try {
        text = await readLimitedText(response, this.maxResponseBytes)
        throwIfTimedOut(deadlineController.signal, request.signal)
      } catch (error) {
        if (deadlineController.signal.aborted && !request.signal?.aborted) {
          throw requestTimeoutError(requestId)
        }
        if (error instanceof HttpClientError) {
          throw new HttpClientError({
            code: error.code,
            message: error.message,
            status: response.status,
            requestId,
            retryAfterSeconds,
            retryable: false,
          })
        }
        if (isAbortError(error)) {
          throw error
        }
        throw new HttpClientError({
          code: 'NETWORK_ERROR',
          message: '读取 Relay Admin 响应失败。',
          status: response.status,
          requestId,
          retryAfterSeconds,
          retryable: response.status >= 500,
        })
      }

      const payload = parseJson(text, response.status, requestId, retryAfterSeconds)
      if (!response.ok) {
        throw createHttpStatusError(response.status, requestId, retryAfterSeconds, payload)
      }

      const parsed = request.schema?.safeParse(payload)
      if (parsed && !parsed.success) {
        throw new HttpClientError({
          code: 'ADMIN_CONTRACT_MISMATCH',
          message: 'Admin 与 Relay 合同不兼容，请更新 Admin 或 Relay。',
          status: response.status,
          requestId,
          retryable: false,
          contractPath: summarizeContractPath(parsed.error),
        })
      }
      return {
        data: parsed ? parsed.data : (payload as T),
        status: response.status,
        headers: response.headers,
      }
    } finally {
      this.clearTimer(timer)
      combinedSignal.dispose()
    }
  }

  private notifyAuthorizationFailure(failure: HttpAuthorizationFailure): void {
    for (const listener of this.authorizationFailureListeners) {
      listener(failure)
    }
  }
}

function normalizeBasePath(basePath: string): string {
  if (!basePath.startsWith('/') || basePath.startsWith('//') || /^\/[^/]*:\/\//.test(basePath)) {
    throw new TypeError('Admin API base path must be relative to the current origin')
  }
  return basePath.replace(/\/+$/, '') || '/'
}

function joinRelativeUrl(basePath: string, path: string): string {
  if (path.includes('\\') || /^(?:[a-z][a-z\d+.-]*:|\/\/)/i.test(path)) {
    throw new TypeError('Admin API requests must use a relative same-origin path')
  }
  const suffix = path.startsWith('/') ? path : `/${path}`
  return `${basePath === '/' ? '' : basePath}${suffix}`
}

function validateRelativePath(path: string): void {
  if (hasControlCharacter(path) || path.includes('#')) {
    throw new TypeError('Admin API path contains a forbidden control character or fragment')
  }
  const pathname = pathWithoutQuery(path)
  if (pathname.includes('\\') || /^(?:[a-z][a-z\d+.-]*:|\/\/)/i.test(pathname)) {
    throw new TypeError('Admin API requests must use a relative same-origin path')
  }
  let decoded = pathname
  for (let depth = 0; depth < 4; depth += 1) {
    if (decoded.includes('\\') || hasDotSegment(decoded) || hasControlCharacter(decoded)) {
      throw new TypeError('Admin API path contains a forbidden traversal segment')
    }
    try {
      const next = decodeURIComponent(decoded)
      if (next === decoded) return
      decoded = next
    } catch {
      throw new TypeError('Admin API path contains invalid percent encoding')
    }
  }
  if (decoded.includes('%') || decoded.includes('\\') || hasDotSegment(decoded)) {
    throw new TypeError('Admin API path contains a forbidden traversal segment')
  }
}

function pathWithoutQuery(path: string): string {
  return path.split('?', 1)[0] ?? path
}

function hasDotSegment(pathname: string): boolean {
  return pathname.split('/').some((segment) => segment === '.' || segment === '..')
}

function hasControlCharacter(value: string): boolean {
  for (const character of value) {
    const codePoint = character.codePointAt(0)
    if (codePoint !== undefined && (codePoint <= 0x1f || codePoint === 0x7f)) return true
  }
  return false
}

function defaultPurpose(method: HttpMethod): HttpRequestPurpose {
  return method === 'GET' ? 'read' : 'mutation'
}

function combineAbortSignals(...signals: Array<AbortSignal | undefined>): {
  signal: AbortSignal
  dispose: () => void
} {
  const active = signals.filter((signal): signal is AbortSignal => signal !== undefined)
  if (active.length === 1) {
    const signal = active.at(0)
    if (!signal) throw new Error('Expected one active abort signal.')
    return { signal, dispose: () => undefined }
  }
  const controller = new AbortController()
  const abort = (signal: AbortSignal) => controller.abort(signal.reason)
  const listeners: Array<{ signal: AbortSignal; listener: () => void }> = []
  for (const signal of active) {
    if (signal.aborted) {
      abort(signal)
      break
    }
    const listener = () => abort(signal)
    signal.addEventListener('abort', listener, { once: true })
    listeners.push({ signal, listener })
  }
  return {
    signal: controller.signal,
    dispose: () => {
      for (const { signal, listener } of listeners) signal.removeEventListener('abort', listener)
    },
  }
}

function throwIfTimedOut(deadlineSignal: AbortSignal, callerSignal?: AbortSignal): void {
  if (deadlineSignal.aborted && !callerSignal?.aborted) {
    throw requestTimeoutError()
  }
}

function requestTimeoutError(requestId: string | null = null): HttpClientError {
  return new HttpClientError({
    code: 'REQUEST_TIMEOUT',
    message: 'Relay Admin 请求超时。',
    requestId,
    retryable: false,
  })
}

function contentLengthExceedsLimit(contentLength: string | null, maxBytes: number): boolean {
  if (!contentLength || !/^\d+$/.test(contentLength.trim())) return false
  const actual = contentLength.trim().replace(/^0+/, '') || '0'
  const limit = String(maxBytes)
  return actual.length > limit.length || (actual.length === limit.length && actual > limit)
}

function summarizeContractPath(error: z.ZodError): string {
  const issue = error.issues[0]
  if (!issue || issue.path.length === 0) return '$'
  return issue.path
    .slice(0, 4)
    .map((segment) =>
      typeof segment === 'number'
        ? `[${segment}]`
        : `.${segment.replace(/[^A-Za-z0-9_-]/g, '?').slice(0, 48)}`,
    )
    .join('')
    .replace(/^\./, '')
}

function isJsonContentType(contentType: string | null): boolean {
  if (!contentType) return false
  const mediaType = contentType.split(';', 1)[0]?.trim().toLowerCase()
  return mediaType === 'application/json' || mediaType?.endsWith('+json') === true
}

function parseRetryAfter(value: string | null): number | null {
  if (!value) return null
  const seconds = Number(value.trim())
  return Number.isFinite(seconds) && seconds >= 0 ? Math.floor(seconds) : null
}

function isAbortError(value: unknown): boolean {
  return value instanceof DOMException && value.name === 'AbortError'
}

async function readLimitedText(response: Response, maxBytes: number): Promise<string> {
  if (!response.body) {
    const text = await response.text()
    if (new TextEncoder().encode(text).byteLength > maxBytes) {
      throw new HttpClientError({ code: 'RESPONSE_TOO_LARGE', message: 'Relay Admin 响应过大。' })
    }
    return text
  }

  const reader = response.body.getReader()
  const decoder = new TextDecoder()
  let bytes = 0
  let text = ''
  try {
    while (true) {
      const chunk = await reader.read()
      if (chunk.done) {
        text += decoder.decode()
        return text
      }
      bytes += chunk.value.byteLength
      if (bytes > maxBytes) {
        await reader.cancel()
        throw new HttpClientError({ code: 'RESPONSE_TOO_LARGE', message: 'Relay Admin 响应过大。' })
      }
      text += decoder.decode(chunk.value, { stream: true })
    }
  } finally {
    reader.releaseLock()
  }
}

function parseJson(
  text: string,
  status: number,
  requestId: string | null,
  retryAfterSeconds: number | null,
): unknown {
  if (text.trim().length === 0) {
    return null
  }
  try {
    return JSON.parse(text) as unknown
  } catch {
    throw new HttpClientError({
      code: 'INVALID_JSON',
      message: 'Relay Admin 返回了无效 JSON。',
      status,
      requestId,
      retryAfterSeconds,
      retryable: status >= 500,
    })
  }
}

function createHttpStatusError(
  status: number,
  requestId: string | null,
  retryAfterSeconds: number | null,
  payload: unknown,
): HttpClientError {
  const safePayload = readSafeErrorFields(payload)
  return new HttpClientError({
    code: 'HTTP_ERROR',
    message: safePayload.message ?? messageForStatus(status),
    status,
    requestId: requestId ?? safePayload.requestId,
    retryAfterSeconds,
    responseCode: safePayload.code,
    retryable: status === 429 || status >= 500,
  })
}

function readSafeErrorFields(value: unknown): {
  code: string | null
  message: string | null
  requestId: string | null
} {
  if (typeof value !== 'object' || value === null) {
    return { code: null, message: null, requestId: null }
  }
  const record = value as Record<string, unknown>
  return {
    code: typeof record.code === 'string' && record.code.length <= 128 ? record.code : null,
    message:
      typeof record.message === 'string' &&
      record.message.length > 0 &&
      record.message.length <= 512
        ? record.message
        : null,
    requestId:
      typeof record.requestId === 'string' && record.requestId.length <= 128
        ? record.requestId
        : null,
  }
}

function messageForStatus(status: number): string {
  switch (status) {
    case 401:
      return '管理认证已失效，请重新载入并通过管理认证。'
    case 403:
      return '当前操作被 Relay Admin 安全策略拒绝。'
    case 404:
      return '请求的 Admin 资源不存在。'
    case 409:
      return 'Relay 状态已变化，请刷新后重试。'
    case 429:
      return '请求过于频繁，请稍后再试。'
    case 503:
      return 'Relay Admin 当前暂不可用。'
    default:
      return 'Relay Admin 请求失败。'
  }
}
