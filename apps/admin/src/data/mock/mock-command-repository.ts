import type { AdminCommandRepository } from '../admin-command-repository'
import type { ActionReceipt, MaintenanceReceipt } from '@/contracts/common'
import type {
  CreateDeviceInput,
  DeviceCredentialReceipt,
  RevokeDeviceInput,
  RotateDeviceInput,
  SetDeviceEnabledInput,
} from '@/contracts/device'
import type {
  DisconnectWechatInput,
  StartWechatLoginInput,
  VerifyWechatLoginInput,
  WechatLoginSession,
  WechatTestReceipt,
} from '@/contracts/wechat'
import { adminErrorSchema, AdminRepositoryError, type AdminError } from '@/contracts/error'
import type { ResultReceipt, ResultRevokeInput } from '@/contracts/result'

import { DEFAULT_MOCK_DELAY, SLOW_MOCK_DELAY, abortableDelay } from './mock-delay'
import { MOCK_NOW, type MockClock } from './mock-clock'
import { getScenarioById, type ScenarioId } from './scenarios'
import { makeSyntheticToken, MOCK_DEVICE_IDS } from './scenarios/fixture-factory'

export interface MockAdminCommandRepositoryOptions {
  clock: MockClock
  getScenarioId: () => ScenarioId
  delaySource?: () => number
  requestIdFactory?: () => string
  failureOverride?: () => boolean
  mutationState?: MockDeviceMutationState
  zeroDelay?: boolean
}

interface MutableDeviceState {
  lastRotatedAt: number | null
  enabled: boolean
  revoked: boolean
}

export interface MockDeviceMutationState {
  devices: Map<string, MutableDeviceState>
  createdDevices: Map<string, CreateDeviceInput & { createdAt: number }>
}

function makeError(
  code: AdminError['code'],
  message: string,
  retryable: boolean,
  requestId: string,
): AdminError {
  return adminErrorSchema.parse({ code, message, retryable, requestId })
}

function baselineDeviceStates(): Array<[string, MutableDeviceState]> {
  return [
    [
      MOCK_DEVICE_IDS.alpha,
      { lastRotatedAt: MOCK_NOW - 20 * 60_000, enabled: true, revoked: false },
    ],
    [
      MOCK_DEVICE_IDS.beta,
      { lastRotatedAt: MOCK_NOW - 45 * 60_000, enabled: true, revoked: false },
    ],
    [MOCK_DEVICE_IDS.gamma, { lastRotatedAt: null, enabled: true, revoked: false }],
  ]
}

export class MockAdminCommandRepository implements AdminCommandRepository {
  private readonly devices: Map<string, MutableDeviceState>
  private readonly createdDevices: Map<string, CreateDeviceInput & { createdAt: number }>
  private wechatLogin: WechatLoginSession | null = null

  private readonly options: Required<
    Pick<
      MockAdminCommandRepositoryOptions,
      'clock' | 'getScenarioId' | 'delaySource' | 'requestIdFactory'
    >
  > &
    Omit<
      MockAdminCommandRepositoryOptions,
      'clock' | 'getScenarioId' | 'delaySource' | 'requestIdFactory'
    >

  constructor(options: MockAdminCommandRepositoryOptions) {
    this.options = {
      clock: options.clock,
      getScenarioId: options.getScenarioId,
      delaySource: options.delaySource ?? Math.random,
      requestIdFactory:
        options.requestIdFactory ?? (() => `mock-req-${Math.random().toString(36).slice(2, 10)}`),
      ...(options.zeroDelay ? { zeroDelay: true } : {}),
      ...(options.failureOverride ? { failureOverride: options.failureOverride } : {}),
    }
    this.devices = options.mutationState?.devices ?? new Map<string, MutableDeviceState>()
    for (const [id, state] of baselineDeviceStates()) {
      if (!this.devices.has(id)) {
        this.devices.set(id, { ...state })
      }
    }
    this.createdDevices = options.mutationState?.createdDevices ?? new Map()
  }

  resetMockState(): void {
    this.devices.clear()
    for (const [id, state] of baselineDeviceStates()) {
      this.devices.set(id, { ...state })
    }
    this.createdDevices.clear()
    this.wechatLogin = null
  }

  private currentScenarioBehavior() {
    return getScenarioById(this.options.getScenarioId()).behavior
  }

  private async applyDelay(signal?: AbortSignal): Promise<void> {
    if (this.options.zeroDelay) {
      return
    }
    const slow = this.currentScenarioBehavior().delayProfile === 'slow'
    const config = slow ? SLOW_MOCK_DELAY : DEFAULT_MOCK_DELAY
    const span = config.maxMs - config.minMs
    const ms =
      span > 0 ? Math.floor(this.options.delaySource() * span) + config.minMs : config.minMs
    await abortableDelay(ms, signal)
  }

  private requestId(): string {
    return this.options.requestIdFactory()
  }

  private nextDeviceId(): string {
    let n = this.devices.size + 1
    let id = `mock-device-new-${n}`
    while (this.devices.has(id)) {
      n += 1
      id = `mock-device-new-${n}`
    }
    return id
  }

  async createDevice(input: CreateDeviceInput): Promise<DeviceCredentialReceipt> {
    await this.applyDelay()
    const id = this.nextDeviceId()
    this.devices.set(id, { lastRotatedAt: null, enabled: true, revoked: false })
    this.createdDevices.set(id, { ...input, createdAt: this.options.clock.now() })
    return {
      receiptId: `mock-recv-${Math.random().toString(36).slice(2, 10)}`,
      action: 'create',
      deviceId: id,
      oneTimeToken: makeSyntheticToken(`create-${id}-${this.options.clock.now()}`),
      issuedAt: this.options.clock.now(),
    }
  }

  async rotateDevice(input: RotateDeviceInput): Promise<DeviceCredentialReceipt> {
    await this.applyDelay()
    const state = this.devices.get(input.deviceId)
    if (!state) {
      throw new AdminRepositoryError(
        makeError('DEVICE_NOT_FOUND', '设备不存在或已删除。', false, this.requestId()),
      )
    }
    if (state.revoked) {
      throw new AdminRepositoryError(
        makeError('DEVICE_REVOKED', '设备已撤销，无法 rotate。', false, this.requestId()),
      )
    }
    state.lastRotatedAt = this.options.clock.now()
    return {
      receiptId: `mock-recv-${Math.random().toString(36).slice(2, 10)}`,
      action: 'rotate',
      deviceId: input.deviceId,
      oneTimeToken: makeSyntheticToken(`rotate-${input.deviceId}-${this.options.clock.now()}`),
      issuedAt: this.options.clock.now(),
    }
  }

  async setDeviceEnabled(input: SetDeviceEnabledInput): Promise<ActionReceipt> {
    await this.applyDelay()
    const state = this.devices.get(input.deviceId)
    if (!state) {
      throw new AdminRepositoryError(
        makeError('DEVICE_NOT_FOUND', '设备不存在或已删除。', false, this.requestId()),
      )
    }
    if (state.revoked) {
      throw new AdminRepositoryError(
        makeError('DEVICE_REVOKED', '设备已撤销，无法修改启用状态。', false, this.requestId()),
      )
    }
    state.enabled = input.enabled
    return {
      receiptId: `mock-recv-${Math.random().toString(36).slice(2, 10)}`,
      action: input.enabled ? 'device.enable' : 'device.disable',
      completedAt: this.options.clock.now(),
    }
  }

  async revokeDevice(input: RevokeDeviceInput): Promise<ActionReceipt> {
    await this.applyDelay()
    const state = this.devices.get(input.deviceId)
    if (!state) {
      throw new AdminRepositoryError(
        makeError('DEVICE_NOT_FOUND', '设备不存在或已删除。', false, this.requestId()),
      )
    }
    state.revoked = true
    state.enabled = false
    return {
      receiptId: `mock-recv-${Math.random().toString(36).slice(2, 10)}`,
      action: 'device.revoke',
      completedAt: this.options.clock.now(),
    }
  }

  async revokeResult(_input: ResultRevokeInput): Promise<ResultReceipt> {
    await this.applyDelay()
    throw new AdminRepositoryError(
      makeError('INVALID_INPUT', '模拟数据中没有可撤销的结果。', false, this.requestId()),
    )
  }

  async startWechatLogin(_input: StartWechatLoginInput): Promise<WechatLoginSession> {
    await this.applyDelay()
    // Login expiration models the browser/server wall clock rather than the
    // frozen scenario timestamps used for deterministic list fixtures.
    const now = Date.now()
    this.wechatLogin = {
      loginId: `mock-login-${Math.random().toString(36).slice(2, 10)}`,
      state: 'waiting_scan',
      qrContent: 'https://mock.promptdock.invalid/login/demo-session',
      expiresAt: now + 3 * 60_000,
      canSubmitVerifyCode: false,
    }
    return this.wechatLogin
  }

  async getWechatLogin(loginId: string): Promise<WechatLoginSession> {
    await this.applyDelay()
    if (!this.wechatLogin || this.wechatLogin.loginId !== loginId) {
      throw new AdminRepositoryError(
        makeError('INVALID_INPUT', '登录会话不存在或已结束。', false, this.requestId()),
      )
    }
    return { ...this.wechatLogin }
  }

  async verifyWechatLogin(input: VerifyWechatLoginInput): Promise<WechatLoginSession> {
    await this.applyDelay()
    if (!this.wechatLogin || this.wechatLogin.loginId !== input.loginId) {
      throw new AdminRepositoryError(
        makeError('INVALID_INPUT', '登录会话不存在或已结束。', false, this.requestId()),
      )
    }
    if (!/^\d{1,16}$/.test(input.code)) {
      throw new AdminRepositoryError(
        makeError('INVALID_INPUT', '验证码格式无效。', false, this.requestId()),
      )
    }
    this.wechatLogin = {
      loginId: this.wechatLogin.loginId,
      state: 'confirmed',
      expiresAt: this.wechatLogin.expiresAt,
      canSubmitVerifyCode: false,
    }
    return { ...this.wechatLogin }
  }

  async cancelWechatLogin(loginId: string): Promise<WechatLoginSession> {
    await this.applyDelay()
    if (!this.wechatLogin || this.wechatLogin.loginId !== loginId) {
      throw new AdminRepositoryError(
        makeError('INVALID_INPUT', '登录会话不存在或已结束。', false, this.requestId()),
      )
    }
    this.wechatLogin = {
      loginId: this.wechatLogin.loginId,
      state: 'cancelled',
      expiresAt: this.wechatLogin.expiresAt,
      canSubmitVerifyCode: false,
    }
    return { ...this.wechatLogin }
  }

  async disconnectWechat(_input: DisconnectWechatInput): Promise<ActionReceipt> {
    await this.applyDelay()
    return {
      receiptId: `mock-recv-${Math.random().toString(36).slice(2, 10)}`,
      action: 'wechat.disconnect',
      completedAt: this.options.clock.now(),
    }
  }

  async sendWechatTest(): Promise<WechatTestReceipt> {
    await this.applyDelay()
    return {
      receiptId: `mock-recv-${Math.random().toString(36).slice(2, 10)}`,
      state: 'accepted_by_relay',
      issuedAt: this.options.clock.now(),
    }
  }

  async runRetention(): Promise<MaintenanceReceipt> {
    await this.applyDelay()
    return {
      receiptId: `mock-recv-${Math.random().toString(36).slice(2, 10)}`,
      action: 'maintenance.retention',
      completedAt: this.options.clock.now(),
      summary: 'mock retention pass complete',
    }
  }
}
