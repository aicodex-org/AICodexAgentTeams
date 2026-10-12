<script setup lang="ts">
import { ZButton, ZCard, ZFormItem, ZInput, ZSegmented } from '@codex-proxy/ui'

import { ArrowRight, KeyRound, ShieldCheck, UserRound } from '@lucide/vue'
import { computed } from 'vue'
import AppBrandMark from '@/components/AppBrandMark.vue'

type LoginRealm = 'admin' | 'key'

const props = defineProps<{
  loading: boolean
  submitDisabled: boolean
}>()

const emit = defineEmits<{
  submit: []
}>()

const realm = defineModel<LoginRealm>('realm', { required: true })
const username = defineModel<string>('username', { required: true })
const password = defineModel<string>('password', { required: true })
const apiKey = defineModel<string>('apiKey', { required: true })

const realmOptions = [
  { label: '管理员登录', value: 'admin', icon: ShieldCheck },
  { label: 'API Key 登录', value: 'key', icon: KeyRound },
]

const isKeyRealm = computed(() => realm.value === 'key')
const submitLabel = computed(() => {
  if (props.loading)
    return '正在登录...'
  return isKeyRealm.value ? '查看用量' : '进入控制台'
})
</script>

<template>
  <ZCard as="form" padding="none" class="login-form" @submit.prevent="emit('submit')">
    <header class="login-heading">
      <div class="login-emblem" aria-hidden="true">
        <span class="emblem-orbit" />
        <AppBrandMark class="login-logo" />
      </div>
      <p class="login-brand">
        AICodexAgentTeams
      </p>
      <h1 id="login-title">
        欢迎回来
      </h1>
      <p class="login-description">
        {{ isKeyRealm ? '连接你的密钥，掌握每一次调用' : '连接你的账号，开启协作之旅' }}
      </p>
    </header>

    <div class="login-body">
      <ZSegmented
        v-model="realm"
        class="login-realms"
        aria-label="选择登录方式"
        :options="realmOptions"
        size="large"
        :disabled="loading"
      />

      <div class="login-fields">
        <template v-if="!isKeyRealm">
          <ZFormItem class="login-field" label="管理员账号" for="admin-username">
            <ZInput
              id="admin-username"
              v-model="username"
              name="username"
              aria-label="管理员账号"
              placeholder="输入管理员账号"
              autocomplete="username"
              autocapitalize="none"
              spellcheck="false"
            >
              <template #prefix>
                <UserRound :size="17" />
              </template>
            </ZInput>
          </ZFormItem>
          <ZFormItem class="login-field" label="登录密码" for="admin-password">
            <ZInput
              id="admin-password"
              v-model="password"
              name="password"
              aria-label="登录密码"
              placeholder="输入登录密码"
              show-password
              autocomplete="current-password"
            >
              <template #prefix>
                <KeyRound :size="17" />
              </template>
            </ZInput>
          </ZFormItem>
        </template>
        <ZFormItem v-else class="login-field" label="API Key" for="client-api-key">
          <ZInput
            id="client-api-key"
            v-model="apiKey"
            name="apiKey"
            aria-label="API Key"
            placeholder="输入你的 API Key"
            show-password
            autocomplete="off"
            autocapitalize="none"
            spellcheck="false"
          >
            <template #prefix>
              <KeyRound :size="17" />
            </template>
          </ZInput>
          <p class="key-hint">
            使用网关密钥查看用量与调用记录
          </p>
        </ZFormItem>
      </div>

      <ZButton
        type="primary"
        size="large"
        native-type="submit"
        class="login-submit"
        :loading="props.loading"
        :disabled="props.submitDisabled"
      >
        <span>{{ submitLabel }}</span>
        <ArrowRight v-if="!loading" :size="17" />
      </ZButton>
    </div>

    <div class="login-signature" aria-hidden="true">
      <span /> LANIAKEA <span />
    </div>
  </ZCard>
</template>

<style scoped>
.login-form {
  --cp-button-primary-bg: var(--cosmic-accent);
  --cp-button-primary-hover-bg: color-mix(in srgb, var(--cosmic-accent) 88%, var(--cp-color-text));
  --cp-button-primary-active-bg: color-mix(in srgb, var(--cosmic-accent) 80%, var(--cp-color-text));
  --cp-button-primary-color: var(--cosmic-on-accent);
  --cp-control-outline: color-mix(in srgb, var(--cosmic-accent) 40%, transparent);
  --cp-input-bg: color-mix(in srgb, var(--cp-color-bg-container) 75%, transparent);
  --cp-input-hover-bg: var(--cp-color-bg-container);
  --cp-input-active-bg: var(--cp-color-bg-container);
  --cp-input-shadow: 0 0 0 1px var(--cp-color-border);
  --cp-input-hover-shadow: 0 0 0 1px color-mix(in srgb, var(--cosmic-accent) 60%, transparent);
  --cp-input-active-shadow: 0 0 0 2px color-mix(in srgb, var(--cosmic-accent) 40%, transparent);
  --cp-border-radius: 10px;
  --cp-border-radius-sm: 10px;
  --cp-control-height: 48px;
  --cp-control-height-lg: 48px;
  position: relative;
  width: 100%;
  overflow: hidden;
  border: 1px solid color-mix(in srgb, var(--cosmic-accent) 24%, var(--cp-color-bg-container));
  border-radius: 24px;
  background: color-mix(in srgb, var(--cp-color-bg-container) 94%, transparent);
  box-shadow: 0 32px 100px -40px color-mix(in srgb, var(--cp-color-text) 26%, transparent);
  backdrop-filter: blur(24px);
  -webkit-backdrop-filter: blur(24px);
}
.login-heading {
  display: grid;
  justify-items: center;
  padding: 36px 36px 28px;
  text-align: center;
  background: radial-gradient(
    ellipse at 50% 0%,
    color-mix(in srgb, var(--cosmic-accent) 9%, transparent),
    transparent 75%
  );
}
.login-emblem {
  position: relative;
  display: grid;
  place-items: center;
  width: 100px;
  height: 80px;
}
.emblem-orbit {
  position: absolute;
  width: 98px;
  height: 56px;
  border: 1px solid color-mix(in srgb, var(--cosmic-accent) 35%, transparent);
  border-radius: 50%;
  transform: rotate(-32deg);
}
.emblem-orbit::after {
  content: '';
  position: absolute;
  top: 3px;
  left: 20px;
  width: 5px;
  height: 5px;
  border-radius: 50%;
  background: var(--cosmic-accent);
  box-shadow: 0 0 12px color-mix(in srgb, var(--cosmic-accent) 50%, transparent);
}
.login-logo {
  position: relative;
  width: 54px;
  height: 54px;
  box-shadow: 0 8px 24px #00000016;
  border-radius: 13px;
}
.login-brand {
  margin: 12px 0 24px;
  font-size: 13px;
  font-weight: 550;
  letter-spacing: 0.2px;
  color: var(--cp-color-text-secondary);
}
.login-heading h1 {
  margin: 0;
  font-size: 32px;
  font-weight: 500;
  line-height: 1.3;
  letter-spacing: 2px;
}
.login-description {
  margin: 12px 0 0;
  font-size: 13px;
  line-height: 1.6;
  color: var(--cp-color-text-secondary);
}
.login-body {
  display: grid;
  gap: 26px;
  padding: 0 40px;
}
.login-realms {
  --cp-input-bg: var(--cp-color-bg-layout);
  width: 100%;
}
.login-realms :deep([aria-checked='true']) {
  color: var(--cosmic-accent);
}
.login-fields {
  display: grid;
  gap: 20px;
}
.login-field :deep(label) {
  font-size: 12px;
  font-weight: 550;
}
.key-hint {
  margin: 10px 0 0;
  font-size: 12px;
  line-height: 1.6;
  color: var(--cp-color-text-secondary);
}
.login-submit {
  width: 100%;
  height: 48px;
  gap: 12px;
  font-size: 14px;
  font-weight: 550;
}
.login-signature {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 12px;
  margin: 28px 40px;
  font-family: 'JetBrains Mono Variable', monospace;
  font-size: 9px;
  letter-spacing: 3px;
  color: var(--cosmic-accent);
}
.login-signature span {
  width: 28px;
  height: 1px;
  background: currentColor;
  opacity: 0.35;
}
@media (max-width: 560px) {
  .login-form {
    border-radius: 20px;
  }
  .login-heading {
    padding: 24px 20px;
  }
  .login-brand {
    margin-bottom: 20px;
  }
  .login-heading h1 {
    font-size: 28px;
  }
  .login-body {
    padding: 0 24px;
    gap: 24px;
  }
  .login-realms :deep(button) {
    padding-inline: 6px;
    font-size: 12px;
  }
  .login-signature {
    margin: 24px;
  }
}
</style>
