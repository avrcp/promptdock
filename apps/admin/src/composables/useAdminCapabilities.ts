import { computed } from 'vue'

import { useAdminSession } from './useAdminSession'
import type { AdminCapability } from '@promptdock/relay-admin-api-generated'

/**
 * Backwards-compatible facade over the shell-level AdminSession.  Existing
 * call sites consume `can(...)`; the session also exposes `loading`,
 * `failed`, `isReadOnly` and `readOnlyReason` so new pages can explain
 * why a button is disabled instead of failing silently.
 */
export function useAdminCapabilities() {
  const session = useAdminSession()

  const loading = computed(() => session.loading.value)
  const failed = computed(() => session.state.value.kind === 'unavailable')
  const isReadOnly = computed(() => session.isReadOnly.value)
  const readOnlyReason = computed(() => session.readOnlyReason.value)
  const meta = computed(() => session.meta.value)

  return {
    meta,
    loading,
    failed,
    isReadOnly,
    readOnlyReason,
    mode: session.mode,
    capabilityReason: (capability: AdminCapability) => session.capabilityReason(capability),
    can: (capability: AdminCapability) => session.can(capability),
    refresh: () => session.refresh(),
  }
}
