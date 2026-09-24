import { z } from 'zod'

import { queueSummarySchema } from './overview'

export const wechatStateSchema = z.enum([
  'disconnected',
  'connected_awaiting_activation',
  'ready',
  'degraded',
  'needs_reconnect',
  'credentials_unreadable',
])
export type WechatState = z.infer<typeof wechatStateSchema>

export const wechatAdminStatusSchema = z
  .object({
    schemaVersion: z.literal(2),
    generatedAt: z.number().int().nonnegative(),
    state: wechatStateSchema,
    accountHint: z.string().nullable(),
    lastPollAt: z.number().int().nonnegative().nullable(),
    lastContextAt: z.number().int().nonnegative().nullable(),
    lastProviderAcceptedAt: z.number().int().nonnegative().nullable(),
    queue: queueSummarySchema,
    lastErrorCode: z.string().nullable(),
  })
  .strict()
export type WechatAdminStatus = z.infer<typeof wechatAdminStatusSchema>

export const channelEventKindSchema = z.enum([
  'poll',
  'context',
  'login',
  'login_cancelled',
  'login_expired',
  'login_failed',
  'disconnect',
  'reconnect',
  'test',
])
export type ChannelEventKind = z.infer<typeof channelEventKindSchema>

export const channelEventItemSchema = z
  .object({
    id: z.string().min(1),
    kind: channelEventKindSchema,
    occurredAt: z.number().int().nonnegative(),
    safeMessage: z.string().min(1),
  })
  .strict()
export type ChannelEventItem = z.infer<typeof channelEventItemSchema>

export const channelEventQuerySchema = z
  .object({
    kinds: z.array(channelEventKindSchema).optional(),
    sinceAt: z.number().int().nonnegative().optional(),
    limit: z.number().int().positive().max(100).default(50),
    cursor: z.string().nullable().optional(),
  })
  .strict()
export type ChannelEventQuery = z.infer<typeof channelEventQuerySchema>

export const wechatLoginStateSchema = z.enum([
  'fetching_qr',
  'waiting_scan',
  'scanned',
  'verify_code_required',
  'refreshing_qr',
  'confirmed',
  'already_connected',
  'expired',
  'cancelled',
  'failed',
])
export type WechatLoginState = z.infer<typeof wechatLoginStateSchema>

export const wechatLoginSessionSchema = z
  .object({
    loginId: z.string().min(1),
    state: wechatLoginStateSchema,
    qrContent: z.string().min(1).optional(),
    expiresAt: z.number().int().nonnegative(),
    canSubmitVerifyCode: z.boolean(),
    errorCode: z.string().min(1).optional(),
  })
  .strict()
  .superRefine((snapshot, context) => {
    const terminal = ['confirmed', 'already_connected', 'expired', 'cancelled', 'failed']
    if (terminal.includes(snapshot.state) && snapshot.qrContent !== undefined) {
      context.addIssue({
        code: z.ZodIssueCode.custom,
        path: ['qrContent'],
        message: 'terminal WeChat login snapshots must not retain QR content',
      })
    }
  })
export type WechatLoginSession = z.infer<typeof wechatLoginSessionSchema>

/** Relay Admin owns cloud-channel login; no Dock credential is accepted. */
export interface StartWechatLoginInput {
  forceFresh: boolean
}

export interface VerifyWechatLoginInput {
  loginId: string
  code: string
}

export const wechatTestReceiptSchema = z
  .object({
    receiptId: z.string().min(1),
    state: z.enum(['accepted_by_relay', 'provider_accepted', 'phone_displayed_unknown']),
    issuedAt: z.number().int().nonnegative(),
  })
  .strict()
export type WechatTestReceipt = z.infer<typeof wechatTestReceiptSchema>

export const disconnectWechatInputSchema = z
  .object({
    reason: z.string().min(1).max(120),
  })
  .strict()
export type DisconnectWechatInput = z.infer<typeof disconnectWechatInputSchema>
