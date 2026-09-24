import type { AdminError } from '@/contracts/error'

import { HttpClientError } from './http-client'

/** Map transport failures to the existing safe UI error model without exposing response bodies. */
export function mapHttpError(error: unknown): AdminError {
  if (!(error instanceof HttpClientError)) {
    return {
      code: 'INTERNAL',
      message: '请求失败，请稍后重试。',
      retryable: false,
      requestId: null,
    }
  }

  return {
    code: mapCode(error),
    message: error.message,
    retryable: isRetryable(error),
    requestId: error.requestId,
  }
}

function isRetryable(error: HttpClientError): boolean {
  return (
    error.retryable ||
    error.responseCode === 'ADMIN_CURSOR_INVALID' ||
    error.responseCode === 'ADMIN_CURSOR_EXPIRED'
  )
}

function mapCode(error: HttpClientError): AdminError['code'] {
  if (error.code === 'ADMIN_CONTRACT_MISMATCH') return 'ADMIN_CONTRACT_MISMATCH'
  switch (error.responseCode) {
    case 'ADMIN_WECHAT_GRANT_REJECTED':
      return 'WECHAT_LOGIN_GRANT_REJECTED'
    case 'ADMIN_WECHAT_LOGIN_NOT_FOUND':
      return 'WECHAT_LOGIN_NOT_FOUND'
    case 'ADMIN_WECHAT_LOGIN_ACCESS_REVOKED':
      return 'WECHAT_LOGIN_ACCESS_REVOKED'
    case 'ADMIN_WECHAT_LOGIN_CONFLICT':
      return 'WECHAT_LOGIN_CONFLICT'
    case 'ADMIN_WECHAT_LOGIN_RATE_LIMITED':
      return 'WECHAT_LOGIN_RATE_LIMITED'
    case 'ADMIN_WECHAT_UNAVAILABLE':
      return 'WECHAT_UNAVAILABLE'
    case 'ADMIN_CURSOR_INVALID':
      return 'CURSOR_INVALID'
    case 'ADMIN_CURSOR_EXPIRED':
      return 'CURSOR_EXPIRED'
  }
  if (error.status === 429) return 'RATE_LIMITED'
  if (error.status === 503 || error.status === 502 || error.status === 504) {
    return 'RELAY_UNAVAILABLE'
  }
  // The transport layer does not know which resource an endpoint addresses.
  // Keep generic HTTP statuses generic until a wire-contract mapper supplies context.
  if (error.responseCode === 'DATABASE_DEGRADED') return 'DATABASE_DEGRADED'
  if (error.responseCode === 'GATEWAY_TIMEOUT') return 'GATEWAY_TIMEOUT'
  return 'INTERNAL'
}
