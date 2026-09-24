import { z, type ZodTypeAny } from 'zod'
import {
  createDeviceRequestWireSchema as createDeviceRequestSchema,
  deviceActionWireSchema,
  deviceCredentialWireSchema,
  disconnectWechatRequestWireSchema as disconnectWechatRequestSchema,
  emptyMutationRequestWireSchema as emptyMutationRequestSchema,
  retentionWireSchema,
  resultReceiptWireSchema,
  resultRevokeRequestWireSchema,
  wechatDisconnectWireSchema,
  wechatLoginStartRequestWireSchema,
  wechatLoginVerifyRequestWireSchema,
  wechatLoginWireSchema,
  wechatTestWireSchema,
  type AdminCapability,
} from '@promptdock/relay-admin-api-generated'

import type { ActionReceipt, MaintenanceReceipt, RequestOptions } from '@/contracts/common'
import { AdminRepositoryError } from '@/contracts/error'
import {
  createDeviceInputSchema,
  type CreateDeviceInput,
  type DeviceCredentialReceipt,
  type RevokeDeviceInput,
  type RotateDeviceInput,
  type SetDeviceEnabledInput,
} from '@/contracts/device'
import type {
  DisconnectWechatInput,
  StartWechatLoginInput,
  VerifyWechatLoginInput,
  WechatLoginSession,
  WechatTestReceipt,
} from '@/contracts/wechat'
import type { ResultReceipt, ResultRevokeInput } from '@/contracts/result'
import type { AdminCommandRepository } from '../admin-command-repository'

import { mapHttpError } from './error-mapper'
import { AdminCapabilityMetadata } from './admin-capability-metadata'
import { HttpClient, HttpClientError } from './http-client'
import { mapWechatLogin } from './mapper'

const REQUIRED_DEVICE_CAPABILITY = 'admin_device_manage_v2'
const REQUIRED_MAINTENANCE_CAPABILITY = 'admin_maintenance_v2'
const REQUIRED_WECHAT_LOGIN_CAPABILITY = 'admin_wechat_login_v2'
const REQUIRED_RESULTS_CAPABILITY = 'admin_results_manage_v2'

export interface HttpAdminCommandRepositoryOptions {
  client?: HttpClient
  capabilityMetadata?: AdminCapabilityMetadata
}

/**
 * Same-origin command adapter.  It intentionally holds no command result or
 * token cache: a credential receipt only exists in the caller's local scope
 * until the UI closes its one-time receipt dialog.
 */
export class HttpAdminCommandRepository implements AdminCommandRepository {
  private readonly client: HttpClient
  private readonly capabilityMetadata: AdminCapabilityMetadata

  constructor(options: HttpAdminCommandRepositoryOptions = {}) {
    this.client = options.client ?? new HttpClient()
    this.capabilityMetadata =
      options.capabilityMetadata ?? new AdminCapabilityMetadata({ client: this.client })
  }

  async createDevice(
    input: CreateDeviceInput,
    options?: RequestOptions,
  ): Promise<DeviceCredentialReceipt> {
    const request = parseCreateDeviceRequest(input)
    return this.mutate(
      REQUIRED_DEVICE_CAPABILITY,
      '/devices',
      deviceCredentialWireSchema,
      request,
      options?.signal,
      201,
      createDeviceRequestSchema,
    )
  }

  async rotateDevice(
    input: RotateDeviceInput,
    options?: RequestOptions,
  ): Promise<DeviceCredentialReceipt> {
    const request = parseDeviceId(input)
    return this.mutate(
      REQUIRED_DEVICE_CAPABILITY,
      `/devices/${encodePathSegment(request.deviceId)}/rotate`,
      deviceCredentialWireSchema,
      {},
      options?.signal,
      200,
    )
  }

  async setDeviceEnabled(
    input: SetDeviceEnabledInput,
    options?: RequestOptions,
  ): Promise<ActionReceipt> {
    const request = parseEnabledInput(input)
    const action = request.enabled ? 'enable' : 'disable'
    return this.mutate(
      REQUIRED_DEVICE_CAPABILITY,
      `/devices/${encodePathSegment(request.deviceId)}/${action}`,
      deviceActionWireSchema,
      {},
      options?.signal,
      200,
    )
  }

  async revokeDevice(input: RevokeDeviceInput, options?: RequestOptions): Promise<ActionReceipt> {
    const request = parseDeviceId(input)
    return this.mutate(
      REQUIRED_DEVICE_CAPABILITY,
      `/devices/${encodePathSegment(request.deviceId)}/revoke`,
      deviceActionWireSchema,
      {},
      options?.signal,
      200,
    )
  }

  async revokeResult(input: ResultRevokeInput, options?: RequestOptions): Promise<ResultReceipt> {
    let request: ResultRevokeInput
    try {
      request = input
      resultRevokeRequestWireSchema.parse({ requestId: request.requestId })
      if (request.resultRowId.length === 0) throw new Error('invalid result row id')
    } catch {
      throw invalidInputError()
    }
    return this.mutate(
      REQUIRED_RESULTS_CAPABILITY,
      `/results/${encodePathSegment(request.resultRowId)}/revoke`,
      resultReceiptWireSchema,
      { requestId: request.requestId },
      options?.signal,
      200,
      resultRevokeRequestWireSchema,
    )
  }

  async runRetention(options?: RequestOptions): Promise<MaintenanceReceipt> {
    const receipt = await this.mutate(
      REQUIRED_MAINTENANCE_CAPABILITY,
      '/maintenance/retention',
      retentionWireSchema,
      {},
      options?.signal,
      200,
    )
    return {
      receiptId: receipt.receiptId,
      action: receipt.action,
      completedAt: receipt.completedAt,
      startedAt: receipt.startedAt,
      outboxDeleted: receipt.outboxDeleted,
      inboundDeleted: receipt.inboundDeleted,
      selectionDeleted: receipt.selectionDeleted,
      summary: `保留清理完成：outbox ${receipt.outboxDeleted}，inbound ${receipt.inboundDeleted}，selection ${receipt.selectionDeleted}。`,
    }
  }

  async startWechatLogin(
    input: StartWechatLoginInput,
    options?: RequestOptions,
  ): Promise<WechatLoginSession> {
    let body: StartWechatLoginInput
    try {
      body = wechatLoginStartRequestWireSchema.parse(input)
    } catch {
      throw invalidInputError()
    }
    return this.loginMutation('/wechat/login', 'POST', body, options?.signal)
  }

  async getWechatLogin(loginId: string, options?: RequestOptions): Promise<WechatLoginSession> {
    const id = parseLoginId(loginId)
    try {
      await this.ensureCapability(REQUIRED_WECHAT_LOGIN_CAPABILITY, options?.signal)
      const response = await this.client.request({
        path: `/wechat/login/${encodePathSegment(id)}`,
        signal: options?.signal,
        schema: wechatLoginWireSchema,
        purpose: 'login-poll',
      })
      if (response.status !== 200) {
        throw new HttpClientError({
          code: 'HTTP_ERROR',
          message: 'Relay Admin 返回了不符合合同的登录响应。',
          status: response.status,
          retryable: false,
        })
      }
      return mapWechatLogin(response.data)
    } catch (error) {
      throw toRepositoryError(error)
    }
  }

  verifyWechatLogin(
    _input: VerifyWechatLoginInput,
    _options?: RequestOptions,
  ): Promise<WechatLoginSession> {
    let body: { code: string }
    try {
      body = wechatLoginVerifyRequestWireSchema.parse({ code: _input.code })
      parseLoginId(_input.loginId)
    } catch {
      return Promise.reject(invalidInputError())
    }
    return this.loginMutation(
      `/wechat/login/${encodePathSegment(_input.loginId)}/verify`,
      'POST',
      body,
      _options?.signal,
    )
  }

  cancelWechatLogin(loginId: string, options?: RequestOptions): Promise<WechatLoginSession> {
    let id: string
    try {
      id = parseLoginId(loginId)
    } catch {
      return Promise.reject(invalidInputError())
    }
    return this.loginMutation(
      `/wechat/login/${encodePathSegment(id)}`,
      'DELETE',
      {},
      options?.signal,
    )
  }

  async disconnectWechat(
    input: DisconnectWechatInput,
    options?: RequestOptions,
  ): Promise<ActionReceipt> {
    let body: DisconnectWechatInput
    try {
      body = disconnectWechatRequestSchema.parse(input)
    } catch {
      throw invalidInputError()
    }
    return this.mutate(
      'admin_wechat_manage_v2',
      '/wechat/disconnect',
      wechatDisconnectWireSchema,
      body,
      options?.signal,
      200,
      disconnectWechatRequestSchema,
    )
  }

  async sendWechatTest(options?: RequestOptions): Promise<WechatTestReceipt> {
    try {
      await this.ensureCapability('admin_wechat_manage_v2', options?.signal)
      const response = await this.client.request({
        path: '/wechat/test',
        method: 'POST',
        body: emptyMutationRequestSchema.parse({}),
        signal: options?.signal,
        schema: wechatTestWireSchema,
      })
      if (response.status !== 202) {
        throw new HttpClientError({
          code: 'HTTP_ERROR',
          message: 'Relay Admin 返回了不符合合同的测试响应。',
          status: response.status,
          retryable: false,
        })
      }
      return response.data
    } catch (error) {
      throw toRepositoryError(error)
    }
  }

  private async mutate<T>(
    capability: AdminCapability,
    path: string,
    schema: z.ZodType<T>,
    body: unknown,
    signal?: AbortSignal,
    expectedStatus = 200,
    bodySchema: ZodTypeAny = emptyMutationRequestSchema,
  ): Promise<T> {
    try {
      await this.ensureCapability(capability, signal)
      const response = await this.client.request({
        path,
        method: 'POST',
        body: bodySchema.parse(body),
        signal,
        schema,
        purpose: 'mutation',
      })
      if (response.status !== expectedStatus) {
        throw new HttpClientError({
          code: 'HTTP_ERROR',
          message: 'Relay Admin 返回了不符合合同的操作响应。',
          status: response.status,
          retryable: false,
        })
      }
      return response.data
    } catch (error) {
      throw toRepositoryError(error)
    }
  }

  private async loginMutation(
    path: string,
    method: 'POST' | 'DELETE',
    body: unknown,
    signal?: AbortSignal,
  ): Promise<WechatLoginSession> {
    try {
      await this.ensureCapability(REQUIRED_WECHAT_LOGIN_CAPABILITY, signal)
      const response = await this.client.request({
        path,
        method,
        body,
        signal,
        schema: wechatLoginWireSchema,
        purpose: path.endsWith('/verify')
          ? 'login-verify'
          : path === '/wechat/login'
            ? 'login-start'
            : 'mutation',
      })
      if (response.status !== 200) {
        throw new HttpClientError({
          code: 'HTTP_ERROR',
          message: 'Relay Admin 返回了不符合合同的登录响应。',
          status: response.status,
          retryable: false,
        })
      }
      return mapWechatLogin(response.data)
    } catch (error) {
      throw toRepositoryError(error)
    }
  }
  private async ensureCapability(capability: AdminCapability, signal?: AbortSignal): Promise<void> {
    throwIfAborted(signal)
    const capabilities = new Set((await this.capabilityMetadata.get()).capabilities)
    throwIfAborted(signal)
    if (!capabilities.has(capability)) {
      throw new AdminRepositoryError({
        code: 'INTERNAL',
        message: 'Relay Admin 未授予当前操作能力。',
        retryable: false,
        requestId: null,
      })
    }
  }
}

function throwIfAborted(signal?: AbortSignal): void {
  if (signal?.aborted) throw signal.reason
}

function parseCreateDeviceRequest(input: CreateDeviceInput): { name: string; scopes: string[] } {
  try {
    const parsed = createDeviceInputSchema.parse(input)
    return createDeviceRequestSchema.parse({ name: parsed.name, scopes: parsed.scopes })
  } catch {
    throw invalidInputError()
  }
}

function parseDeviceId(input: RotateDeviceInput | RevokeDeviceInput): { deviceId: string } {
  if (!input || typeof input.deviceId !== 'string' || input.deviceId.length === 0) {
    throw invalidInputError()
  }
  return { deviceId: input.deviceId }
}

function parseEnabledInput(input: SetDeviceEnabledInput): SetDeviceEnabledInput {
  if (
    !input ||
    typeof input.deviceId !== 'string' ||
    input.deviceId.length === 0 ||
    typeof input.enabled !== 'boolean'
  ) {
    throw invalidInputError()
  }
  return input
}

function parseLoginId(input: string): string {
  try {
    return z.string().uuid().parse(input)
  } catch {
    throw invalidInputError()
  }
}

function encodePathSegment(value: string): string {
  return encodeURIComponent(value)
}

function invalidInputError(): AdminRepositoryError {
  return new AdminRepositoryError({
    code: 'INVALID_INPUT',
    message: '操作参数无效。',
    retryable: false,
    requestId: null,
  })
}

function toRepositoryError(error: unknown): Error {
  if (error instanceof AdminRepositoryError) return error
  if (error instanceof DOMException && error.name === 'AbortError') return error
  if (error instanceof HttpClientError) {
    return new AdminRepositoryError(mapHttpError(error))
  }
  if (error instanceof z.ZodError) {
    return new AdminRepositoryError({
      code: 'INTERNAL',
      message: 'Relay Admin 返回了不符合合同的响应。',
      retryable: false,
      requestId: null,
    })
  }
  return new AdminRepositoryError({
    code: 'INTERNAL',
    message: '请求失败，请稍后重试。',
    retryable: false,
    requestId: null,
  })
}
