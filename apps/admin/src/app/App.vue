<script setup lang="ts">
import AppShell from './AppShell.vue'
import { useAdminSession } from '@/composables/useAdminSession'

// Install the shell-level admin session exactly once.  All mutation pages
// resolve the same capability state, so a single failed meta load can be
// explained (and retried) in one place instead of silently disabling
// buttons in three different views.
useAdminSession()
</script>

<template>
  <AppShell />
</template>
