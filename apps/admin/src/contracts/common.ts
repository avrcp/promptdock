import { z } from 'zod'

export const componentHealthSchema = z.enum([
  'healthy',
  'degraded',
  'failed',
  'disabled',
  'unknown',
])
export type ComponentHealth = z.infer<typeof componentHealthSchema>

export const healthToneSchema = z.enum(['success', 'info', 'warning', 'danger', 'muted'])
export type HealthTone = z.infer<typeof healthToneSchema>

export const issueSeveritySchema = z.enum(['info', 'warning', 'error'])
export type IssueSeverity = z.infer<typeof issueSeveritySchema>

export const issueComponentSchema = z.enum([
  'relay',
  'database',
  'wechat',
  'gateway',
  'outbox',
  'inbound',
])
export type IssueComponent = z.infer<typeof issueComponentSchema>

/** A currently observable, safe operational condition. Not an incident history. */
export const currentAlertSchema = z
  .object({
    id: z.string().min(1),
    code: z.string().min(1),
    severity: issueSeveritySchema,
    component: issueComponentSchema,
    message: z.string().min(1),
    observedAt: z.number().int().nonnegative(),
  })
  .strict()
export type CurrentAlert = z.infer<typeof currentAlertSchema>

export const actionReceiptSchema = z
  .object({
    receiptId: z.string().min(1),
    action: z.string().min(1),
    completedAt: z.number().int().nonnegative(),
  })
  .strict()
export type ActionReceipt = z.infer<typeof actionReceiptSchema>

export const maintenanceReceiptSchema = z
  .object({
    receiptId: z.string().min(1),
    action: z.string().min(1),
    completedAt: z.number().int().nonnegative(),
    summary: z.string().min(1),
    // Present for the real Relay retention receipt. Optional keeps existing
    // mock scenarios source-compatible while preserving every server count.
    outboxDeleted: z.number().int().nonnegative().optional(),
    inboundDeleted: z.number().int().nonnegative().optional(),
    selectionDeleted: z.number().int().nonnegative().optional(),
    startedAt: z.number().int().nonnegative().optional(),
  })
  .strict()
export type MaintenanceReceipt = z.infer<typeof maintenanceReceiptSchema>

export function pageSchema<T extends z.ZodTypeAny>(item: T) {
  return z
    .object({
      items: z.array(item),
      nextCursor: z.string().nullable(),
      total: z.number().int().nonnegative().nullable(),
      generatedAt: z.number().int().nonnegative(),
    })
    .strict()
}

export interface Page<T> {
  items: T[]
  nextCursor: string | null
  total: number | null
  generatedAt: number
}

export const requestOptionsSchema = z
  .object({
    signal: z.instanceof(globalThis.AbortSignal).optional(),
    /** Local cache control only; it is never serialized onto the Relay wire. */
    force: z.boolean().optional(),
  })
  .optional()
export type RequestOptions = { signal?: AbortSignal; force?: boolean }
