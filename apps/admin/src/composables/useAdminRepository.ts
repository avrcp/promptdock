import { inject, provide, type App, type InjectionKey } from 'vue'

import type { AdminRepositoryBundle } from '@/data/create-admin-repository'
import { getFallbackRepository } from '@/composables/admin-repository-fallback'

export const AdminRepositoryKey: InjectionKey<AdminRepositoryBundle> = Symbol('AdminRepository')

/** Install a repository in a component subtree. */
export function provideAdminRepository(repository: AdminRepositoryBundle): void {
  provide(AdminRepositoryKey, repository)
}

/** Install a repository at the application root when creating the Vue app. */
export function installAdminRepository(app: App, repository: AdminRepositoryBundle): void {
  app.provide(AdminRepositoryKey, repository)
}

/** Resolve the runtime repository; the mock singleton remains a test/dev fallback. */
export function useAdminRepository(): AdminRepositoryBundle {
  const repository = inject(AdminRepositoryKey, null)
  if (repository) {
    return repository
  }

  return getFallbackRepository(__ADMIN_PRODUCTION_BUILD__ ? 'production' : 'mock')
}
