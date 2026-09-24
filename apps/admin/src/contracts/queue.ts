import { z } from 'zod'

export const deliveryOriginSchema = z.enum(['device', 'system', 'admin'])
export type DeliveryOrigin = z.infer<typeof deliveryOriginSchema>

export const deliveryKindSchema = z.enum(['run_event', 'test', 'interactive_reply', 'activation'])
export type DeliveryKind = z.infer<typeof deliveryKindSchema>

export const deliveryStateSchema = z.enum([
  'queued',
  'sending',
  'retrying',
  'blocked',
  'accepted',
  'failed',
  'cancelled',
  'expired',
])
export type DeliveryState = z.infer<typeof deliveryStateSchema>

export const deliveryListItemSchema = z
  .object({
    id: z.string().min(1),
    origin: deliveryOriginSchema,
    originLabel: z.string().min(1),
    kind: deliveryKindSchema,
    state: deliveryStateSchema,
    priority: z.number().int().min(0).max(255),
    attemptCount: z.number().int().nonnegative(),
    segmentCount: z.number().int().min(1).max(128).optional(),
    acceptedSegments: z.number().int().min(0).max(128).optional(),
    errorCode: z.string().nullable(),
    createdAt: z.number().int().nonnegative(),
    updatedAt: z.number().int().nonnegative(),
  })
  .strict()
export type DeliveryListItem = z.infer<typeof deliveryListItemSchema>

export const deliveryListQuerySchema = z
  .object({
    state: deliveryStateSchema.optional(),
    origin: deliveryOriginSchema.optional(),
    sinceAt: z.number().int().nonnegative().optional(),
    untilAt: z.number().int().nonnegative().optional(),
    limit: z.number().int().positive().max(100).default(50),
    cursor: z.string().nullable().optional(),
  })
  .strict()
export type DeliveryListQuery = z.infer<typeof deliveryListQuerySchema>

export const interactiveReplyListItemSchema = z
  .object({
    id: z.string().min(1),
    commandRef: z.string().min(1),
    targetFingerprint: z.string().min(1),
    state: deliveryStateSchema,
    occurredAt: z.number().int().nonnegative(),
    errorCode: z.string().nullable(),
  })
  .strict()
export type InteractiveReplyListItem = z.infer<typeof interactiveReplyListItemSchema>

export const interactiveReplyListQuerySchema = z
  .object({
    state: deliveryStateSchema.optional(),
    limit: z.number().int().positive().max(100).default(50),
    cursor: z.string().nullable().optional(),
  })
  .strict()
export type InteractiveReplyListQuery = z.infer<typeof interactiveReplyListQuerySchema>

export const inboundCommandSchema = z.enum([
  'help',
  'list_devices',
  'list_jobs',
  'list_recent',
  'list_failed',
  'next_page',
  'get_status',
  'get_detail',
  'get_tree',
  'unknown',
])
export type InboundCommand = z.infer<typeof inboundCommandSchema>

export const inboundStateSchema = z.enum([
  'received',
  'dispatching',
  'waiting_gateway',
  'reply_queued',
  'expired',
  'dead_letter',
])
export type InboundState = z.infer<typeof inboundStateSchema>

export const inboundCommandListItemSchema = z
  .object({
    id: z.string().min(1),
    command: inboundCommandSchema,
    senderHint: z.string().min(1),
    state: inboundStateSchema,
    attemptCount: z.number().int().nonnegative(),
    errorCode: z.string().nullable(),
    createdAt: z.number().int().nonnegative(),
    expiresAt: z.number().int().nonnegative(),
    updatedAt: z.number().int().nonnegative(),
  })
  .strict()
export type InboundCommandListItem = z.infer<typeof inboundCommandListItemSchema>

export const inboundCommandListQuerySchema = z
  .object({
    command: inboundCommandSchema.optional(),
    state: inboundStateSchema.optional(),
    sinceAt: z.number().int().nonnegative().optional(),
    limit: z.number().int().positive().max(100).default(50),
    cursor: z.string().nullable().optional(),
  })
  .strict()
export type InboundCommandListQuery = z.infer<typeof inboundCommandListQuerySchema>
