import { createApp } from 'vue'
import App from './App.vue'

import './styles/global.css'
// Navigation Workbench tokens: imported after global.css so these values win.
import './design-system/tokens.css'
import './design-system/components.css'

createApp(App).mount('#app')
