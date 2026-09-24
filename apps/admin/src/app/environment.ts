export type DataSourceMode = 'mock' | 'production'

export interface RuntimeEnvironment {
  mode: DataSourceMode
  buildVersion: string
  buildCommit: string
  adminApiMajor: number
}

function readDataSourceMode(): DataSourceMode {
  const raw = import.meta.env.VITE_DATA_SOURCE_MODE
  if (raw === 'production') {
    return 'production'
  }
  return 'mock'
}

function readBuildVersion(): string {
  return typeof __ADMIN_BUILD_VERSION__ === 'string' && __ADMIN_BUILD_VERSION__.length > 0
    ? __ADMIN_BUILD_VERSION__
    : '0.6.0-rc.1'
}

function readBuildCommit(): string {
  return typeof __ADMIN_BUILD_COMMIT__ === 'string' && /^[0-9a-f]{40}$/.test(__ADMIN_BUILD_COMMIT__)
    ? __ADMIN_BUILD_COMMIT__
    : 'unknown'
}

function readAdminApiMajor(): number {
  const raw = __ADMIN_API_MAJOR__
  return Number.isSafeInteger(raw) && raw > 0 ? raw : 1
}

export const runtimeEnvironment: RuntimeEnvironment = {
  mode: readDataSourceMode(),
  buildVersion: readBuildVersion(),
  buildCommit: readBuildCommit(),
  adminApiMajor: readAdminApiMajor(),
}
