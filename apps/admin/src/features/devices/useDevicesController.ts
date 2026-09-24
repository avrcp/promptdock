import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { useRoute } from 'vue-router'

import { useAdminCapabilities } from '@/composables/useAdminCapabilities'
import { useAdminRepository } from '@/composables/useAdminRepository'
import { useAsyncResource } from '@/composables/useAsyncResource'
import { usePageQuery } from '@/composables/usePageQuery'
import {
  DEVICE_SCOPE_VALUES,
  deviceStateFilterSchema,
  type DeviceScope,
  type CreateDeviceInput,
  type DeviceCredentialReceipt,
  type DeviceDetail,
  type DeviceListItem,
  type DeviceStateFilter,
} from '@/contracts/device'
import { toAdminError, type AdminError } from '@/contracts/error'

export type DeviceMutationAction = 'rotate' | 'enable' | 'disable' | 'revoke'

export interface DeviceReceiptState {
  receipt: DeviceCredentialReceipt
  deviceName: string
}

export const DEVICE_STATE_FILTERS: ReadonlyArray<{
  value: DeviceStateFilter | null
  label: string
}> = [
  { value: null, label: '全部' },
  ...deviceStateFilterSchema.options.map((value) => ({
    value,
    label:
      value === 'online'
        ? '在线'
        : value === 'offline'
          ? '离线'
          : value === 'disabled'
            ? '已禁用'
            : '已撤销',
  })),
]

export function useDevicesController() {
  const route = useRoute()
  const repository = useAdminRepository()
  const { can: canCapability, capabilityReason } = useAdminCapabilities()
  const query = usePageQuery({ state: null, search: null, device: null })

  const stateFilter = computed<DeviceStateFilter | null>({
    get: () => {
      const parsed = deviceStateFilterSchema.safeParse(query.values.state)
      return parsed.success ? parsed.data : null
    },
    set: (value) => query.set('state', value),
  })
  const searchInput = computed<string>({
    get: () => query.values.search ?? '',
    set: (value) => query.set('search', value || null),
  })
  const debouncedSearch = ref(searchInput.value)
  let debounceTimer: ReturnType<typeof setTimeout> | null = null
  watch(searchInput, (value) => {
    if (debounceTimer) clearTimeout(debounceTimer)
    debounceTimer = setTimeout(() => {
      debouncedSearch.value = value
    }, 200)
  })

  const listResource = useAsyncResource<Awaited<ReturnType<typeof repository.read.listDevices>>>({
    fetcher: (signal) => {
      const params: Parameters<typeof repository.read.listDevices>[0] = { limit: 50, cursor: null }
      const search = debouncedSearch.value.trim()
      if (stateFilter.value) params.state = stateFilter.value
      if (search) params.search = search
      return repository.read.listDevices(params, { signal })
    },
  })
  watch([stateFilter, debouncedSearch], () => void listResource.refresh('initial'))
  const devices = computed(() => listResource.data.value?.items ?? [])
  const loading = computed(() => listResource.loading.value && listResource.data.value === null)

  const selectedDeviceId = computed<string | null>({
    get: () => query.values.device ?? null,
    set: (value) => query.set('device', value, 'push'),
  })
  const detailResource = useAsyncResource<DeviceDetail | null>({
    immediate: false,
    fetcher: (signal) =>
      selectedDeviceId.value
        ? repository.read.getDevice(selectedDeviceId.value, { signal })
        : Promise.resolve(null),
  })
  const drawerOpen = computed<boolean>({
    get: () => selectedDeviceId.value !== null,
    set: (open) => {
      if (!open) selectedDeviceId.value = null
    },
  })
  watch(
    selectedDeviceId,
    (id) => {
      detailResource.data.value = null
      detailResource.error.value = null
      if (id) void detailResource.refresh('initial')
    },
    { immediate: true },
  )

  const createDialogOpen = ref(false)
  const createName = ref('')
  const createScopes = ref<DeviceScope[]>(['gateway:connect'])
  const createInFlight = ref(false)
  const createError = ref<AdminError | null>(null)

  const mutationAction = ref<DeviceMutationAction | null>(null)
  const mutationDevice = ref<DeviceListItem | null>(null)
  const mutationInFlight = ref(false)
  const mutationError = ref<AdminError | null>(null)

  const receiptState = ref<DeviceReceiptState | null>(null)
  const receiptDialogOpen = computed<boolean>({
    get: () => receiptState.value !== null,
    set: (open) => {
      if (!open) clearCredentialReceipt()
    },
  })
  const receiptActionHint = computed(() =>
    receiptState.value?.receipt.action === 'rotate'
      ? '关闭后无法再次查看，请立即复制保存到安全位置。若网络结果未知，当前 token 状态也未知；确认设备状态后可再次 Rotate，旧 token 会被原子替换。'
      : '关闭后无法再次查看，请立即复制保存到安全位置。',
  )

  const canManage = computed(() => canCapability('admin_device_manage_v2'))
  const manageReason = computed(() => capabilityReason('admin_device_manage_v2'))

  function openCreate(): void {
    if (!canManage.value) return
    createName.value = ''
    createScopes.value = ['gateway:connect']
    createError.value = null
    createDialogOpen.value = true
  }

  async function submitCreate(): Promise<void> {
    if (createInFlight.value) return
    const name = createName.value.trim()
    if (!name || createScopes.value.length === 0) {
      createError.value = {
        code: 'INVALID_INPUT',
        message: name ? '至少需要一个 scope。' : '设备名称不能为空。',
        retryable: false,
        requestId: null,
      }
      return
    }
    const input: CreateDeviceInput = {
      name,
      scopes: createScopes.value.filter((scope) => DEVICE_SCOPE_VALUES.includes(scope)),
    }
    createInFlight.value = true
    createError.value = null
    try {
      const receipt = await repository.command.createDevice(input)
      receiptState.value = { receipt, deviceName: name }
      createDialogOpen.value = false
      await listResource.refresh('refresh')
    } catch (error) {
      createError.value = toAdminError(error, {
        code: 'INTERNAL',
        message: '新建设备失败，请重试。',
        retryable: true,
        requestId: null,
      })
    } finally {
      createInFlight.value = false
    }
  }

  function openMutation(device: DeviceListItem, action: DeviceMutationAction): void {
    if (!canManage.value) return
    mutationAction.value = action
    mutationDevice.value = device
    mutationError.value = null
  }
  function closeMutation(): void {
    if (mutationInFlight.value) return
    mutationAction.value = null
    mutationDevice.value = null
    mutationError.value = null
  }
  async function submitMutation(): Promise<void> {
    const action = mutationAction.value
    const device = mutationDevice.value
    if (!action || !device || mutationInFlight.value) return
    mutationInFlight.value = true
    mutationError.value = null
    try {
      if (action === 'rotate') {
        const receipt = await repository.command.rotateDevice({ deviceId: device.id })
        receiptState.value = { receipt, deviceName: device.name }
      } else if (action === 'enable' || action === 'disable') {
        await repository.command.setDeviceEnabled({
          deviceId: device.id,
          enabled: action === 'enable',
        })
      } else {
        await repository.command.revokeDevice({ deviceId: device.id })
      }
      mutationAction.value = null
      mutationDevice.value = null
      await listResource.refresh('refresh')
      if (selectedDeviceId.value === device.id) await detailResource.refresh('refresh')
    } catch (error) {
      mutationError.value = toAdminError(error, {
        code: 'INTERNAL',
        message: '操作失败，请重试。',
        retryable: true,
        requestId: null,
      })
    } finally {
      mutationInFlight.value = false
    }
  }

  function clearCredentialReceipt(): void {
    receiptState.value = null
  }
  function clearFilters(): void {
    stateFilter.value = null
    searchInput.value = ''
  }
  async function refresh(): Promise<void> {
    await listResource.refresh('refresh')
  }
  function openDevice(id: string): void {
    selectedDeviceId.value = id
  }

  watch(
    () => route.fullPath,
    (currentPath, previousPath) => {
      if (currentPath !== previousPath) clearCredentialReceipt()
    },
  )
  onBeforeUnmount(() => {
    if (debounceTimer) clearTimeout(debounceTimer)
    clearCredentialReceipt()
  })

  return {
    stateFilter,
    searchInput,
    listResource,
    devices,
    loading,
    drawerOpen,
    detailResource,
    selectedDetail: computed(() => detailResource.data.value),
    detailLoading: computed(
      () => detailResource.loading.value && detailResource.data.value === null,
    ),
    createDialogOpen,
    createName,
    createScopes,
    createInFlight,
    createError,
    mutationAction,
    mutationDevice,
    mutationInFlight,
    mutationError,
    receiptState,
    receiptDialogOpen,
    receiptActionHint,
    canManage,
    manageReason,
    openCreate,
    submitCreate,
    openMutation,
    closeMutation,
    submitMutation,
    clearCredentialReceipt,
    clearFilters,
    refresh,
    openDevice,
  }
}
