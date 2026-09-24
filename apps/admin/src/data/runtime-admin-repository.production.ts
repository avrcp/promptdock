import type { AdminRepositoryBundle } from './create-admin-repository'
import { HttpAdminCommandRepository } from './http/http-admin-command-repository'
import { HttpAdminReadRepository } from './http/http-admin-read-repository'
import { HttpClient } from './http/http-client'
import { AdminCapabilityMetadata } from './http/admin-capability-metadata'

/** Production-only bootstrap. Vite aliases this module so Mock fixtures and
 * scenario code cannot enter a production artifact. */
export function createRuntimeAdminRepository(): AdminRepositoryBundle {
  const client = new HttpClient()
  const capabilityMetadata = new AdminCapabilityMetadata({ client })
  const clock: AdminRepositoryBundle['clock'] = {
    now: () => Date.now(),
    setNow: () => undefined,
    advance: () => undefined,
    reset: () => undefined,
  }
  return {
    read: new HttpAdminReadRepository({ client, capabilityMetadata }),
    command: new HttpAdminCommandRepository({ client, capabilityMetadata }),
    capabilityMetadata,
    clock,
    reset: () => undefined,
  }
}
