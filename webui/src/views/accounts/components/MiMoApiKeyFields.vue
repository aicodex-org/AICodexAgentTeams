<script setup lang="ts">
import type { ApiKeyAccountForm } from '../utils/upstreamApiKey'
import { ZFormItem, ZSegmented, ZSelect } from '@codex-proxy/ui'
import { computed } from 'vue'
import { inferMiMoAccessMode, miMoBaseUrlAfterModeChange, miMoCluster, miMoDefaultBaseUrl } from '../utils/mimo'

defineProps<{ disabled?: boolean }>()
const form = defineModel<ApiKeyAccountForm>({ required: true })
const mode = computed({
  get: () => form.value.access_mode ?? inferMiMoAccessMode({ base_url: form.value.base_url }),
  set: (value: string) => {
    if (value !== 'api_key' && value !== 'token_plan')
      return
    const baseUrl = miMoBaseUrlAfterModeChange(form.value.base_url, mode.value, value)
    form.value = { ...form.value, access_mode: value, base_url: baseUrl, transport: 'http' }
  },
})
const modeOptions = [{ label: '按量 API', value: 'api_key' }, { label: 'Token Plan', value: 'token_plan' }]
const clusterOptions = computed(() => [
  ...(mode.value === 'token_plan'
    ? [{ label: '中国', value: 'cn' }, { label: '新加坡', value: 'sgp' }, { label: '欧洲（阿姆斯特丹）', value: 'ams' }]
    : [{ label: '官方统一地址', value: 'api' }]),
  { label: '自定义地址', value: 'custom' },
])
const cluster = computed({
  get: () => miMoCluster(form.value.base_url, mode.value),
  set: (value: string) => {
    if (value === 'custom') {
      if (cluster.value !== 'custom')
        form.value.base_url = ''
      return
    }
    if (value === 'api' || value === 'cn' || value === 'sgp' || value === 'ams')
      form.value.base_url = miMoDefaultBaseUrl(mode.value, value === 'api' ? 'cn' : value)
  },
})
</script>

<template>
  <ZFormItem label="接入方式">
    <ZSegmented v-model="mode" :options="modeOptions" aria-label="MiMo 接入方式" :disabled="disabled" />
  </ZFormItem>
  <ZFormItem label="地区 / 集群">
    <ZSelect v-model="cluster" class="w-full" :options="clusterOptions" aria-label="MiMo 地区或集群" :disabled="disabled" />
  </ZFormItem>
</template>
