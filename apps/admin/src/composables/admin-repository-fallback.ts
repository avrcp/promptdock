import type { AdminRepositoryBundle } from '@/data/create-admin-repository'
import type { DataSourceMode } from '@/app/environment'
import { adminRepository } from './useScenarioSelector'

export function getFallbackRepository(_mode: DataSourceMode): AdminRepositoryBundle {
  return adminRepository
}
