import { describe, expect, it } from 'vitest'

import { createAdminRepository, createRuntimeAdminRepository } from './create-admin-repository'

describe('runtime repository selection', () => {
  it('never silently falls back to mock data in production mode', async () => {
    const repository = createRuntimeAdminRepository('production')

    await expect(repository.read.getOverview()).rejects.toMatchObject({
      name: 'AdminRepositoryError',
      adminError: {
        code: 'INTERNAL',
        retryable: true,
      },
    })
  })

  it('injects a production repository without changing the mock factory', () => {
    const productionRepository = createAdminRepository({ testMode: true })
    expect(createRuntimeAdminRepository('production', { productionRepository })).toBe(
      productionRepository,
    )
  })
})
