import { describe, expect, it } from 'vitest'

import { buildQueryString, withQuery } from './query'

describe('HTTP query construction', () => {
  it('encodes primitives, repeats wechat event kinds, and omits nullish values', () => {
    expect(
      buildQueryString({
        search: 'a/b',
        limit: 50,
        includeDisabled: false,
        cursor: null,
        kinds: ['poll', 'test'],
      }),
    ).toBe('?search=a%2Fb&limit=50&includeDisabled=false&kinds=poll&kinds=test')
  })

  it('appends to an existing query without re-encoding the path', () => {
    expect(withQuery('/devices?view=compact', { cursor: 'a b' })).toBe(
      '/devices?view=compact&cursor=a+b',
    )
  })
})
