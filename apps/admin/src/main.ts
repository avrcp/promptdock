import { createApp } from 'vue'

import App from '@/app/App.vue'
import { router } from '@/app/router'
import { createRuntimeAdminRepository } from '@/data/runtime-admin-repository'
import { installAdminRepository } from '@/composables/useAdminRepository'

import '@/design-system/fonts.css'
import '@/design-system/tokens.css'
import '@/design-system/reset.css'
import '@/design-system/utilities.css'
import '@/design-system/motion.css'

const app = createApp(App)
app.use(router)
installAdminRepository(
  app,
  createRuntimeAdminRepository(__ADMIN_PRODUCTION_BUILD__ ? 'production' : 'mock'),
)
app.mount('#app')
