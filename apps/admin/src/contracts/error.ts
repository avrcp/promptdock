import { z } from 'zod'

export const adminErrorCodeSchema = z.enum([
  'RELAY_UNAVAILABLE',
  'WECHAT_RECONNECT_REQUIRED',
  'DATABASE_DEGRADED',
  'GATEWAY_TIMEOUT',
  'OUTBOX_BACKLOG',
  'RATE_LIMITED',
  'DEVICE_NOT_FOUND',
  'DEVICE_REVOKED',
  'WECHAT_LOGIN_GRANT_REJECTED',
  'WECHAT_LOGIN_NOT_FOUND',
  'WECHAT_LOGIN_ACCESS_REVOKED',
  'WECHAT_LOGIN_CONFLICT',
  'WECHAT_LOGIN_RATE_LIMITED',
  'WECHAT_UNAVAILABLE',
  'CURSOR_INVALID',
  'CURSOR_EXPIRED',
  'ADMIN_CONTRACT_MISMATCH',
  'INVALID_INPUT',
  'INTERNAL',
])
export type AdminErrorCode = z.infer<typeof adminErrorCodeSchema>

export const adminErrorSchema = z
  .object({
    code: adminErrorCodeSchema,
    message: z.string().min(1),
    retryable: z.boolean(),
    requestId: z.string().nullable(),
  })
  .strict()
export type AdminError = z.infer<typeof adminErrorSchema>

export class AdminRepositoryError extends Error {
  readonly adminError: AdminError

  constructor(adminError: AdminError) {
    super(adminError.message)
    this.name = 'AdminRepositoryError'
    this.adminError = adminError
  }
}

export function isAdminError(value: unknown): value is AdminError {
  return adminErrorSchema.safeParse(value).success
}

/**
 * Normalize errors at the repository/presentation boundary.  Production
 * repositories wrap the safe contract in AdminRepositoryError; UI code must
 * not lose its code, retryability, or request id while unwrapping it.
 */
export function toAdminError(error: unknown, fallback: AdminError): AdminError {
  if (error instanceof AdminRepositoryError) return error.adminError
  if (isAdminError(error)) return error
  if (error instanceof DOMException && error.name === 'AbortError') return fallback
  if (error instanceof Error && error.name === 'AbortError') return fallback
  return fallback
}
