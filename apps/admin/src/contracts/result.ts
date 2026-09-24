import { z } from 'zod'
import {
  resultDetailWireSchema,
  resultListItemWireSchema,
  resultReceiptWireSchema,
  resultsQueryWireSchema,
} from '@promptdock/relay-admin-api-generated'

export const resultListItemSchema = resultListItemWireSchema
export type ResultListItem = z.infer<typeof resultListItemSchema>

export const resultDetailSchema = resultDetailWireSchema
export type ResultDetail = z.infer<typeof resultDetailSchema>

export const resultReceiptSchema = resultReceiptWireSchema
export type ResultReceipt = z.infer<typeof resultReceiptSchema>

export const resultListQuerySchema = resultsQueryWireSchema
export type ResultListQuery = z.infer<typeof resultListQuerySchema>

export const resultRevokeInputSchema = z
  .object({ resultRowId: z.string().min(1), requestId: z.string().min(1).max(128) })
  .strict()
export type ResultRevokeInput = z.infer<typeof resultRevokeInputSchema>
