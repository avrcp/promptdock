import type { AdminReadRepository } from './admin-read-repository'
import type { AdminCommandRepository } from './admin-command-repository'
import type { DataSourceMode } from '@/app/environment'
import { AdminRepositoryError } from '@/contracts/error'
import { HttpAdminReadRepository } from './http/http-admin-read-repository'
import { HttpAdminCommandRepository } from './http/http-admin-command-repository'
import type { AdminCapabilityMetadata } from './http/admin-capability-metadata'
import { AdminCapabilityMetadata as CapabilityMetadata } from './http/admin-capability-metadata'
import { HttpClient } from './http/http-client'

import {
  MockAdminReadRepository,
  type MockAdminReadRepositoryOptions,
} from './mock/mock-admin-repository'
import {
  MockAdminCommandRepository,
  type MockDeviceMutationState,
} from './mock/mock-command-repository'
import { createMockClock, type MockClock } from './mock/mock-clock'
import type { ScenarioId } from './mock/scenarios'

export interface CreateAdminRepositoryOptions {
  initialScenario?: ScenarioId
  getScenarioId?: () => ScenarioId
  clock?: MockClock
  delaySource?: () => number
  requestIdFactory?: () => string
  failureOverride?: MockAdminReadRepositoryOptions['failureOverride']
  commandFailureOverride?: () => boolean
  testMode?: boolean
}

export interface RuntimeRepositoryOptions {
  /** A real HTTP bundle may be supplied by an integration test or host shell. */
  productionRepository?: AdminRepositoryBundle
  mockOptions?: CreateAdminRepositoryOptions
}

export interface AdminRepositoryBundle {
  read: AdminReadRepository
  command: AdminCommandRepository
  clock: MockClock
  reset: () => void
  /** Production-only shared capability lifecycle. Mock bundles intentionally omit it. */
  capabilityMetadata?: AdminCapabilityMetadata
}

/**
 * Select the runtime boundary without silently serving mock data in production.
 * The production bundle is deliberately injected so the runtime cannot
 * silently fall back to mock data when the real same-origin API is absent.
 */
export function createRuntimeAdminRepository(
  mode: DataSourceMode,
  options: RuntimeRepositoryOptions = {},
): AdminRepositoryBundle {
  if (mode === 'production') {
    return options.productionRepository ?? createHttpProductionRepository()
  }
  return createAdminRepository(options.mockOptions)
}

export function createAdminRepository(
  options: CreateAdminRepositoryOptions = {},
): AdminRepositoryBundle {
  const clock = options.clock ?? createMockClock()
  const scenarioRef: { current: ScenarioId } = {
    current: options.initialScenario ?? 'healthy',
  }
  const getScenarioId = options.getScenarioId ?? (() => scenarioRef.current)
  const delaySource = options.testMode ? () => 0 : (options.delaySource ?? Math.random)
  const requestIdFactory =
    options.requestIdFactory ?? (() => `mock-req-${Math.random().toString(36).slice(2, 10)}`)
  const mutationState: MockDeviceMutationState = {
    devices: new Map(),
    createdDevices: new Map(),
  }

  const read = new MockAdminReadRepository({
    clock,
    initialScenario: scenarioRef.current,
    getScenarioId,
    delaySource,
    ...(options.testMode ? { zeroDelay: true } : {}),
    requestIdFactory,
    ...(options.failureOverride ? { failureOverride: options.failureOverride } : {}),
    mutationState,
  })
  const command = new MockAdminCommandRepository({
    clock,
    getScenarioId,
    delaySource: options.testMode ? () => 0 : (options.delaySource ?? Math.random),
    ...(options.testMode ? { zeroDelay: true } : {}),
    requestIdFactory,
    ...(options.commandFailureOverride ? { failureOverride: options.commandFailureOverride } : {}),
    mutationState,
  })
  return { read, command, clock, reset: () => command.resetMockState() }
}

function createUnconfiguredProductionRepository(): AdminRepositoryBundle {
  const error = () =>
    new AdminRepositoryError({
      code: 'INTERNAL',
      message: '真实 Admin HTTP repository 尚未配置。',
      retryable: false,
      requestId: null,
    })
  const fail = <T>(): Promise<T> => Promise.reject(error())
  const read: AdminReadRepository = {
    getOverview: () => fail(),
    listDevices: () => fail(),
    getDevice: () => fail(),
    getWechatStatus: () => fail(),
    listChannelEvents: () => fail(),
    listDeliveries: () => fail(),
    listInteractiveReplies: () => fail(),
    listInboundCommands: () => fail(),
    listResults: () => fail(),
    getSystemSnapshot: () => fail(),
  }
  const command: AdminCommandRepository = {
    createDevice: () => fail(),
    rotateDevice: () => fail(),
    setDeviceEnabled: () => fail(),
    revokeDevice: () => fail(),
    revokeResult: () => fail(),
    startWechatLogin: () => fail(),
    getWechatLogin: () => fail(),
    verifyWechatLogin: () => fail(),
    cancelWechatLogin: () => fail(),
    disconnectWechat: () => fail(),
    sendWechatTest: () => fail(),
    runRetention: () => fail(),
  }
  return { read, command, clock: createMockClock(), reset: () => undefined }
}

function createHttpProductionRepository(): AdminRepositoryBundle {
  const unavailable = createUnconfiguredProductionRepository()
  const client = new HttpClient()
  const capabilityMetadata = new CapabilityMetadata({ client })
  return {
    ...unavailable,
    read: new HttpAdminReadRepository({ client, capabilityMetadata }),
    command: new HttpAdminCommandRepository({ client, capabilityMetadata }),
    capabilityMetadata,
  }
}
