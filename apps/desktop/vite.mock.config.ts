import { fileURLToPath, URL } from 'node:url'
import vue from '@vitejs/plugin-vue'
import { defineConfig } from 'vitest/config'

/**
 * 设计走查专用 dev server：
 *   npx vite --config vite.mock.config.ts
 *
 * 唯一区别于 vite.config.ts：把 @tauri-apps/api/core 指向 scripts/dev/mock-tauri.ts，
 * 使全部视图可以在纯浏览器中渲染并截图。不参与生产构建与测试。
 */
export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  resolve: {
    alias: {
      '@tauri-apps/api/core': fileURLToPath(
        new URL('./scripts/dev/mock-tauri.ts', import.meta.url),
      ),
    },
  },
  server: {
    port: 5199,
    strictPort: true,
  },
})
