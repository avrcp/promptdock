import { describe, it, expect } from 'vitest'

import { adminOverviewSchema } from '@/contracts/overview'
import { deviceListItemSchema, deviceDetailSchema } from '@/contracts/device'
import { wechatAdminStatusSchema, channelEventItemSchema } from '@/contracts/wechat'
import {
  deliveryListItemSchema,
  inboundCommandListItemSchema,
  interactiveReplyListItemSchema,
} from '@/contracts/queue'
import { systemSnapshotSchema } from '@/contracts/system'
import { currentAlertSchema } from '@/contracts/common'

import { getAllScenarios } from './index'

describe('all scenarios', () => {
  for (const scenario of getAllScenarios()) {
    describe(`scenario ${scenario.id}`, () => {
      it('has matching label and description in SCENARIO_DESCRIPTORS', () => {
        expect(scenario.label.length).toBeGreaterThan(0)
        expect(scenario.description.length).toBeGreaterThan(0)
      })

      it('overview parses with adminOverviewSchema', () => {
        expect(() => adminOverviewSchema.parse(scenario.data.overview)).not.toThrow()
      })

      it('wechat status parses', () => {
        expect(() => wechatAdminStatusSchema.parse(scenario.data.wechat)).not.toThrow()
      })

      it('every device parses and device details cover the same ids', () => {
        const ids: string[] = []
        for (const item of scenario.data.devices) {
          expect(() => deviceListItemSchema.parse(item)).not.toThrow()
          ids.push(item.id)
        }
        for (const id of ids) {
          const detail = scenario.data.deviceDetails[id]
          expect(detail, `missing detail for ${id}`).toBeDefined()
          if (detail) {
            expect(() => deviceDetailSchema.parse(detail)).not.toThrow()
            expect(detail.id).toBe(id)
          }
        }
      })

      it('every channel event parses', () => {
        for (const event of scenario.data.channelEvents) {
          expect(() => channelEventItemSchema.parse(event)).not.toThrow()
        }
      })

      it('every delivery parses', () => {
        for (const item of scenario.data.deliveries) {
          expect(() => deliveryListItemSchema.parse(item)).not.toThrow()
        }
      })

      it('every interactive reply parses', () => {
        for (const item of scenario.data.replies) {
          expect(() => interactiveReplyListItemSchema.parse(item)).not.toThrow()
        }
      })

      it('every inbound command parses', () => {
        for (const item of scenario.data.inbound) {
          expect(() => inboundCommandListItemSchema.parse(item)).not.toThrow()
        }
      })

      it('system snapshot parses', () => {
        expect(() => systemSnapshotSchema.parse(scenario.data.system)).not.toThrow()
      })

      it('every current alert parses', () => {
        for (const alert of scenario.data.alerts) {
          expect(() => currentAlertSchema.parse(alert)).not.toThrow()
        }
      })

      it('overview.currentAlerts is consistent with data.alerts', () => {
        for (const alert of scenario.data.alerts) {
          expect(scenario.data.overview.currentAlerts).toContainEqual(alert)
        }
      })
    })
  }

  it('exposes exactly the 12 fixed scenarios', () => {
    expect(getAllScenarios()).toHaveLength(12)
  })
})
