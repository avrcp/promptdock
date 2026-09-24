import { z } from 'zod'

import { componentHealthSchema, currentAlertSchema } from './common'

export const buildSummarySchema = z
  .object({
    relayVersion: z.string().min(1),
    gitCommit: z.string().min(1).nullable(),
    buildTime: z.string().min(1).nullable(),
    rustVersion: z.string().min(1).nullable(),
    apiVersion: z.string().min(1),
    gatewayVersion: z.string().min(1),
  })
  .strict()
export type BuildSummary = z.infer<typeof buildSummarySchema>

export const databaseSizeBucketSchema = z.enum([
  'lt_10mb',
  '10_to_100mb',
  '100_to_500mb',
  'gt_500mb',
])
export type DatabaseSizeBucket = z.infer<typeof databaseSizeBucketSchema>

export const databaseSummarySchema = z
  .object({
    schemaIdentity: z.string().min(1),
    schemaRevision: z.number().int().nonnegative(),
    integrityStatus: z.enum(['ok', 'degraded', 'failed']),
    walStatus: z.enum(['enabled', 'disabled', 'unknown']),
    foreignKeysEnabled: z.boolean(),
    poolHealth: componentHealthSchema,
    sizeBucket: databaseSizeBucketSchema,
    lastRetentionPassAt: z.number().int().nonnegative().nullable(),
  })
  .strict()
export type DatabaseSummary = z.infer<typeof databaseSummarySchema>

export const workerStateSchema = z.enum([
  'running',
  'idle',
  'degraded',
  'failed',
  'disabled',
  'stopping',
])
export type WorkerState = z.infer<typeof workerStateSchema>

export const workerSummarySchema = z
  .object({
    name: z.string().min(1),
    state: workerStateSchema,
    lastTickAt: z.number().int().nonnegative().nullable(),
    detail: z.string().nullable(),
  })
  .strict()
export type WorkerSummary = z.infer<typeof workerSummarySchema>

export const retentionResultSchema = z.enum(['success', 'partial', 'failed', 'not_observed'])
export type RetentionResult = z.infer<typeof retentionResultSchema>

export const retentionSummarySchema = z
  .object({
    acceptedDays: z.number().int().nonnegative(),
    deadLetterDays: z.number().int().nonnegative(),
    inboundTerminalDays: z.number().int().nonnegative(),
    inboundExpiredDays: z.number().int().nonnegative(),
    lastRunAt: z.number().int().nonnegative().nullable(),
    lastResult: retentionResultSchema,
  })
  .strict()
export type RetentionSummary = z.infer<typeof retentionSummarySchema>

export const safeConfigurationSummarySchema = z
  .object({
    wechatEnabled: z.boolean(),
    publicBindClass: z.enum(['loopback', 'container']),
    adminBindClass: z.literal('loopback'),
    adminMode: z.enum(['read_only', 'operator']),
    forwardedHttpsObserved: z.boolean(),
    featureFlags: z.array(z.string().min(1)),
  })
  .strict()
export type SafeConfigurationSummary = z.infer<typeof safeConfigurationSummarySchema>

export const systemSnapshotSchema = z
  .object({
    schemaVersion: z.literal(2),
    generatedAt: z.number().int().nonnegative(),
    build: buildSummarySchema,
    database: databaseSummarySchema,
    workers: z.array(workerSummarySchema),
    retention: retentionSummarySchema,
    configuration: safeConfigurationSummarySchema,
    currentAlerts: z.array(currentAlertSchema),
  })
  .strict()
export type SystemSnapshot = z.infer<typeof systemSnapshotSchema>
