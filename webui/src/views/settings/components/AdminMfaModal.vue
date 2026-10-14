<script setup lang="ts">
import type { MfaSetup } from '@/api/modules/login'
import { ZButton, ZCheckbox, ZDialog, ZFormItem, ZInput, ZNotification } from '@codex-proxy/ui'
import { computed, onBeforeUnmount, shallowRef, useId, watch } from 'vue'
import { useRouter } from 'vue-router'
import { confirmMfa, disableMfa, setupMfa } from '@/api/modules/login'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useAuthStore } from '@/stores/modules/auth'

const props = defineProps<{ mode: 'enable' | 'disable' | 'recovery' }>()
const open = defineModel<boolean>({ default: false })
const password = shallowRef('')
const code = shallowRef('')
const setup = shallowRef<MfaSetup>()
const step = shallowRef<'password' | 'scan' | 'recovery'>('password')
const saved = shallowRef(false)
const formId = useId()
const router = useRouter()
const auth = useAuthStore()
const { loading, run } = useAsyncAction()
let revision = 0
const title = computed(() => props.mode === 'disable' ? '关闭双重验证' : props.mode === 'recovery' ? '重新生成恢复码' : '开启双重验证')
const canSubmit = computed(() => !!password.value && (props.mode === 'disable'
  ? !!code.value.trim()
  : step.value !== 'recovery' || (saved.value && !!code.value.trim())))

function clear() {
  revision += 1
  password.value = ''
  code.value = ''
  setup.value = undefined
  step.value = 'password'
  saved.value = false
}

watch(open, clear)
onBeforeUnmount(clear)

async function finish() {
  auth.invalidateSession()
  open.value = false
  clear()
  ZNotification.success({ message: '安全设置已更新，请重新登录' })
  await router.replace('/login')
}

function downloadCodes() {
  if (!setup.value)
    return
  const blob = new Blob([`AICodexAgentTeams 恢复码\n每个恢复码只能使用一次，需同时提供管理员密码\n\n${setup.value.recoveryCodes.join('\n')}\n`], { type: 'text/plain;charset=utf-8' })
  const url = URL.createObjectURL(blob)
  const link = document.createElement('a')
  link.href = url
  link.download = 'AICodexAgentTeams-recovery-codes.txt'
  link.click()
  URL.revokeObjectURL(url)
}

async function submit() {
  if (!canSubmit.value)
    return
  if (step.value === 'scan') {
    step.value = 'recovery'
    return
  }
  const current = revision
  await run(async () => {
    if (props.mode === 'disable') {
      await disableMfa(password.value, code.value.trim())
      if (current === revision)
        await finish()
    }
    else if (!setup.value) {
      const data = await setupMfa(password.value)
      if (current !== revision || !open.value)
        return
      setup.value = data
      step.value = data.qrCode ? 'scan' : 'recovery'
    }
    else {
      await confirmMfa(password.value, code.value.trim(), setup.value.setupId)
      if (current === revision)
        await finish()
    }
  })
}
</script>

<template>
  <ZDialog
    v-model="open"
    :title="title"
    width="min(30rem, calc(100vw - 2rem))"
    :show-close="!loading"
    :close-on-click-modal="!loading"
    :close-on-press-escape="!loading"
  >
    <form :id="formId" class="grid gap-4" @submit.prevent="submit">
      <p v-if="mode === 'disable'" class="m-0 text-cp-text-secondary">
        关闭后将仅使用密码登录，所有管理员需重新登录
      </p>
      <p v-else class="m-0 text-cp-sm text-cp-text-secondary" role="status">
        {{ step === 'password' ? '验证当前密码' : step === 'scan' ? '使用验证器扫描二维码，绑定信息 10 分钟内有效' : '保存恢复码，再输入验证码确认，完成后所有管理员需重新登录' }}
      </p>
      <ZFormItem v-if="step === 'password'" label="当前密码" :for="`${formId}-password`" required>
        <ZInput :id="`${formId}-password`" v-model="password" aria-label="当前密码" type="password" autocomplete="current-password" :disabled="loading" maxlength="4096" />
      </ZFormItem>
      <template v-if="step === 'scan' && setup">
        <img :src="`data:image/png;base64,${setup.qrCode}`" alt="验证器绑定二维码" width="208" height="208" class="mx-auto rounded-cp">
        <ZFormItem label="无法扫码时手动输入密钥" :for="`${formId}-secret`">
          <ZInput :id="`${formId}-secret`" :model-value="setup.secret" aria-label="无法扫码时手动输入密钥" readonly class="font-mono" />
        </ZFormItem>
      </template>
      <template v-if="step === 'recovery' && setup">
        <p class="m-0 text-cp-sm text-cp-text-secondary">
          丢失验证器时可使用恢复码，每个只能使用一次
          <span v-if="mode === 'recovery'">，确认后旧恢复码立即失效</span>
        </p>
        <pre class="m-0 overflow-x-auto rounded-cp bg-cp-fill-quaternary p-3 text-xs leading-6 select-all">{{ setup.recoveryCodes.join('\n') }}</pre>
        <ZButton @click="downloadCodes">
          下载恢复码
        </ZButton>
        <ZCheckbox v-model="saved" :disabled="loading" label="我已安全保存恢复码" />
      </template>
      <ZFormItem v-if="mode === 'disable' || step === 'recovery'" :label="mode === 'enable' ? '6 位验证码' : '验证码或现有恢复码'" :for="`${formId}-code`" required>
        <ZInput :id="`${formId}-code`" v-model="code" :aria-label="mode === 'enable' ? '6 位验证码' : '验证码或现有恢复码'" autocomplete="one-time-code" autocapitalize="none" :spellcheck="false" :maxlength="mode === 'enable' ? 6 : 32" :disabled="loading" />
      </ZFormItem>
    </form>
    <template #footer>
      <ZButton :disabled="loading" @click="open = false">
        取消
      </ZButton>
      <ZButton native-type="submit" :form="formId" type="primary" :loading="loading" :disabled="!canSubmit">
        {{ mode === 'disable' ? '关闭并重新登录' : step === 'password' ? '下一步' : step === 'scan' ? '我已添加验证器' : '确认并重新登录' }}
      </ZButton>
    </template>
  </ZDialog>
</template>
