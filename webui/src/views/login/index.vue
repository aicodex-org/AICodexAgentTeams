<script setup lang="ts">
import { ArrowLeft } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'
import { RouterLink, useRoute, useRouter } from 'vue-router'

import { useAuthStore } from '@/stores/modules/auth'

import LoginBackground from './components/LoginBackground.vue'
import LoginPanel from './components/LoginPanel.vue'
import LoginThemeToggle from './components/LoginThemeToggle.vue'
import '@/components/cosmic/surface.css'

type LoginRealm = 'admin' | 'key'

const route = useRoute()
const router = useRouter()
const authStore = useAuthStore()

const realm = shallowRef<LoginRealm>(normalizeRealm(window.history.state?.loginMode))
const username = shallowRef('')
const password = shallowRef('')
const apiKey = shallowRef('')
const secondFactor = shallowRef('')
const requiresTwoFactor = shallowRef(false)

watch([realm, username, password], () => {
  requiresTwoFactor.value = false
  secondFactor.value = ''
})
const loginPending = shallowRef(false)

const canSubmit = computed(() => realm.value === 'key'
  ? Boolean(apiKey.value.trim())
  : Boolean(username.value.trim() && password.value && (!requiresTwoFactor.value || secondFactor.value.trim())))
const loginLoading = computed(() => authStore.loading || loginPending.value)
const submitDisabled = computed(() => loginLoading.value || !canSubmit.value)

async function handleSubmit(): Promise<void> {
  if (!canSubmit.value || loginPending.value)
    return

  loginPending.value = true
  const selectedRealm = realm.value
  const result = await authStore.login(selectedRealm === 'key'
    ? { mode: 'key', apiKey: apiKey.value.trim() }
    : {
        mode: 'admin',
        username: username.value.trim(),
        password: password.value,
        secondFactor: secondFactor.value.trim() || undefined,
      })

  if (!result) {
    loginPending.value = false
    return
  }

  if ('requiresTwoFactor' in result) {
    requiresTwoFactor.value = true
    loginPending.value = false
    return
  }
  password.value = ''
  secondFactor.value = ''
  try {
    if (result.role === 'admin')
      await router.push(resolveDestination())
    else
      await router.push('/key-usage')
  }
  finally {
    if (router.currentRoute.value.path === '/login')
      loginPending.value = false
  }
}

function normalizeRealm(value: unknown): LoginRealm {
  return value === 'key' ? 'key' : 'admin'
}

function resolveDestination(): string {
  const requested = route.query.redirect
  if (typeof requested !== 'string' || !requested.startsWith('/') || requested.startsWith('//'))
    return '/dashboard'
  return requested
}
</script>

<template>
  <main class="login-page cosmic-surface">
    <LoginBackground />
    <header class="login-navigation">
      <RouterLink to="/" class="back-home">
        <ArrowLeft :size="16" /> 返回主页
      </RouterLink>
      <LoginThemeToggle />
    </header>
    <section class="login-content" aria-label="AICodexAgentTeams 登录">
      <LoginPanel
        v-model:realm="realm"
        v-model:username="username"
        v-model:password="password"
        v-model:api-key="apiKey"
        v-model:second-factor="secondFactor"
        :requires-two-factor="requiresTwoFactor"
        :loading="loginLoading"
        :submit-disabled="submitDisabled"
        @submit="handleSubmit"
      />
    </section>
  </main>
</template>

<style scoped>
.login-page {
  position: relative;
  isolation: isolate;
  min-height: 100dvh;
}
.login-navigation {
  position: absolute;
  inset: 0 0 auto;
  z-index: 2;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 26px 4.5vw;
}
.back-home {
  display: inline-flex;
  align-items: center;
  gap: 10px;
  font-size: 13px;
  color: var(--cp-color-text-secondary);
}
.back-home:hover {
  color: var(--cosmic-accent);
}
.login-content {
  position: relative;
  display: grid;
  place-items: center;
  min-height: 100dvh;
  padding: 100px 24px;
}
.login-content :deep(.login-form) {
  width: min(480px, 100%);
}
@media (max-width: 560px) {
  .login-navigation {
    padding: 20px;
  }
  .login-content {
    padding: 88px 20px;
  }
}
</style>
