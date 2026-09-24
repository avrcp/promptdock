import { describe, expect, it } from 'vitest'
import { parseHookHealth, type HookHealth } from './hookHealth'

function health(overrides: Partial<HookHealth> = {}): HookHealth {
  return {
    runtimeEpoch: 'epoch-a',
    revision: 1,
    observedAt: Date.now(),
    fresh: true,
    observationEnabled: true,
    hookSourcePath: 'C:\\isolated\\hooks.json',
    userStateSourcePath: 'C:\\isolated\\config.toml',
    sourceResolution: 'confirmed',
    compatibility: 'validated',
    hostBuild: 'test-build',
    installation: 'current',
    registrationId: 'registration-a',
    definitionFingerprint: 'fingerprint-a',
    diagnosticCode: null,
    handlers: [
      {
        event: 'UserPromptSubmit',
        required: true,
        configured: true,
        observationCurrent: false,
        trust: 'matching_record',
        evidenceSource: 'persisted_user_config',
        lastErrorCode: null,
        lastObservedAt: null,
        observedRegistrationId: null,
      },
    ],
    verification: {
      state: 'not_started',
      verificationId: null,
      validatedAt: null,
      validatedDefinitionFingerprint: null,
      hostAttribution: 'unknown',
      instruction: null,
    },
    ...overrides,
  }
}

describe('Hook health DTO boundary', () => {
  it('rejects unknown closed-enum values instead of presenting them as healthy', () => {
    expect(parseHookHealth({ ...health(), installation: 'future_success' })).toBeNull()
    expect(
      parseHookHealth({
        ...health(),
        handlers: [{ ...health().handlers[0]!, trust: 'future_trust' }],
      }),
    ).toBeNull()
  })

  it('rejects negative and non-integer revisions', () => {
    expect(parseHookHealth({ ...health(), revision: -1 })).toBeNull()
    expect(parseHookHealth({ ...health(), revision: 1.5 })).toBeNull()
  })
})
