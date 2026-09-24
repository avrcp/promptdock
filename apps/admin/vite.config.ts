import { fileURLToPath, URL } from 'node:url'

import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

const sourceRoot = fileURLToPath(new URL('./src', import.meta.url))
const productionDataSource = process.env.VITE_DATA_SOURCE_MODE === 'production'
const buildVersion = process.env.VITE_BUILD_VERSION ?? '0.6.0-rc.1'
const buildCommit = process.env.VITE_BUILD_COMMIT ?? 'unknown'
const adminApiMajor = Number(process.env.VITE_ADMIN_API_MAJOR ?? '2')

export default defineConfig({
  plugins: [vue()],
  define: {
    __ADMIN_PRODUCTION_BUILD__: JSON.stringify(productionDataSource),
    __ADMIN_MOCK_BUILD__: JSON.stringify(!productionDataSource),
    __ADMIN_BUILD_VERSION__: JSON.stringify(buildVersion),
    __ADMIN_BUILD_COMMIT__: JSON.stringify(buildCommit),
    __ADMIN_API_MAJOR__: JSON.stringify(adminApiMajor),
  },
  resolve: {
    alias: [
      {
        find: '@/data/runtime-admin-repository',
        replacement: fileURLToPath(
          new URL(
            productionDataSource
              ? './src/data/runtime-admin-repository.production.ts'
              : './src/data/runtime-admin-repository.ts',
            import.meta.url,
          ),
        ),
      },
      {
        find: '@/composables/admin-repository-fallback',
        replacement: fileURLToPath(
          new URL(
            productionDataSource
              ? './src/composables/admin-repository-fallback.production.ts'
              : './src/composables/admin-repository-fallback.ts',
            import.meta.url,
          ),
        ),
      },
      {
        find: '@/components/DevScenarioSelector.vue',
        replacement: fileURLToPath(
          new URL(
            productionDataSource
              ? './src/components/DevScenarioSelector.production.vue'
              : './src/components/DevScenarioSelector.vue',
            import.meta.url,
          ),
        ),
      },
      { find: '@', replacement: sourceRoot },
    ],
  },
  server: {
    port: 5173,
    strictPort: true,
    // Keep the browser on the Vite origin in development.  The Relay still
    // receives the exact loopback Origin and does not need CORS support.
    proxy: {
      '/admin/api': {
        target: 'http://127.0.0.1:8081',
        changeOrigin: false,
      },
    },
  },
  preview: {
    port: 4173,
    strictPort: true,
  },
  build: {
    target: 'es2022',
    sourcemap: !productionDataSource,
  },
})
