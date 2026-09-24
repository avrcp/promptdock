import type { AdminRepositoryBundle } from '@/data/create-admin-repository'
import type { DataSourceMode } from '@/app/environment'

export function getFallbackRepository(_mode: DataSourceMode): AdminRepositoryBundle {
  throw new Error('Production Admin repository was not provided at the application root.')
}
