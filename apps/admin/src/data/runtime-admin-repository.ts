import type { AdminRepositoryBundle } from './create-admin-repository'
import { adminRepository } from '@/composables/useScenarioSelector'
import type { DataSourceMode } from '@/app/environment'

/**
 * Mock builds intentionally share the scenario-aware singleton with the
 * development selector and static acceptance previews. The production alias
 * replaces this module entirely, keeping mock fixtures out of that artifact.
 */
export function createRuntimeAdminRepository(_mode: DataSourceMode): AdminRepositoryBundle {
  return adminRepository
}
