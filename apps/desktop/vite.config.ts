import { defineConfig } from 'vitest/config'
import vue from '@vitejs/plugin-vue'

// Tauri expects a fixed dev server. clearScreen is disabled so logs don't
// interfere with Tauri's own process output.
export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    watch: {
      // Cargo continuously replaces PE/PDB artifacts while `tauri dev` is
      // compiling. On Windows (notably Node 24), asking Vite to watch those
      // locked files can terminate the dev server with EBUSY and leave the
      // WebView pointing at a dead URL.
      ignored: ['**/src-tauri/target/**'],
    },
  },
  test: {
    environment: 'jsdom',
    include: ['src/**/*.test.ts'],
  },
})
