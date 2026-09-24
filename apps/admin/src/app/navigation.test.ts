import { describe, it, expect } from 'vitest'

import { navigationItems } from './navigation'

describe('navigation', () => {
  it('contains the fixed top-level items in order', () => {
    expect(navigationItems.map((item) => item.path)).toEqual([
      '/overview',
      '/devices',
      '/wechat',
      '/queue',
      '/results',
      '/system',
    ])
  })

  it('uses lucide components as icons', () => {
    for (const item of navigationItems) {
      expect(item.icon).toBeDefined()
      expect(typeof item.icon).toBe('function')
    }
  })
})
