<script setup lang="ts">
import type { MfaStatus } from '@/api/modules/login'
import { ZButton, ZCard } from '@codex-proxy/ui'
import { ShieldCheck } from '@lucide/vue'
import { onMounted, shallowRef } from 'vue'
import { getMfaStatus } from '@/api/modules/login'
import { useAsyncAction } from '@/composables/useAsyncAction'
import AdminMfaModal from './AdminMfaModal.vue'

const status = shallowRef<MfaStatus>()
const open = shallowRef(false)
const action = shallowRef<'enable' | 'disable' | 'recovery'>('enable')

function show(mode: 'enable' | 'disable' | 'recovery') {
  action.value = mode
  open.value = true
}
const { loading, run } = useAsyncAction()

function loadStatus() {
  return run(async () => {
    status.value = await getMfaStatus()
  })
}

onMounted(loadStatus)
</script>

<template>
  <ZCard title="双重验证（2FA）" description="管理员登录时，使用验证器动态码进行二次验证">
    <template #actions>
      <ZButton v-if="!status" :loading="loading" @click="loadStatus">
        重试
      </ZButton>
      <template v-else>
        <ZButton v-if="status.enabled" :disabled="loading" @click="show('recovery')">
          重新生成恢复码
        </ZButton>
        <ZButton :disabled="loading" @click="show(status.enabled ? 'disable' : 'enable')">
          <template #icon>
            <ShieldCheck class="size-4" aria-hidden="true" />
          </template>
          {{ status.enabled ? '关闭双重验证' : '开启双重验证' }}
        </ZButton>
      </template>
    </template>
    <div class="flex min-h-16 max-w-6xl items-center gap-3 rounded-cp bg-cp-fill-quaternary px-4 py-3">
      <ShieldCheck class="size-5 shrink-0 text-cp-primary-text" aria-hidden="true" />
      <div class="min-w-0">
        <p class="m-0 font-bold text-cp-text" role="status">
          {{ loading ? '正在读取状态...' : status ? (status.enabled ? '已开启' : '未开启') : '状态读取失败' }}
        </p>
        <p class="mt-1.5 mb-0 text-cp-sm text-cp-text-secondary">
          {{ status?.enabled ? `剩余 ${status.recoveryCodesRemaining} 个一次性恢复码` : '支持 Google Authenticator、Microsoft Authenticator 等验证器' }}
        </p>
      </div>
    </div>
    <p class="mt-3 mb-0 text-cp-sm text-cp-text-secondary">
      仅保护管理员网页登录，管理 API Key 仍可独立访问管理接口
    </p>
  </ZCard>
  <AdminMfaModal v-if="status" v-model="open" :mode="action" />
</template>
