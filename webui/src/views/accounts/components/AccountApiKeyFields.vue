<script setup lang="ts">
import type { ApiKeyAccountForm } from '../utils/upstreamApiKey'
import { ZCheckbox, ZFormItem, ZInput, ZSegmented } from '@codex-proxy/ui'
import DeepSeekPricingFields from './DeepSeekPricingFields.vue'
import MiMoApiKeyFields from './MiMoApiKeyFields.vue'

defineProps<{ disabled?: boolean, editing?: boolean, provider?: string }>()
const form = defineModel<ApiKeyAccountForm>({ required: true })
const protocolOptions = [{ value: 'openai-chat', label: 'Chat Completions' }, { value: 'openai', label: 'Responses' }, { value: 'anthropic', label: 'Messages' }]
function toggleProtocol(protocol: string, checked: boolean) {
  const values = form.value.protocols ?? []
  form.value.protocols = checked ? [...values, protocol] : values.filter(value => value !== protocol)
}
const transportOptions = [
  { label: 'HTTP / SSE', value: 'http' },
  { label: 'WS 优先', value: 'prefer_websocket' },
]
</script>

<template>
  <div class="grid gap-4">
    <ZFormItem v-if="!editing" label="账号名称" required>
      <ZInput v-model="form.name" placeholder="输入上游名称" :disabled="disabled" />
    </ZFormItem>
    <MiMoApiKeyFields v-if="provider === 'mimo'" v-model="form" :disabled="disabled" />
    <ZFormItem label="Base URL" required>
      <template #extra>
        <span class="text-cp-xs text-cp-text-quaternary">{{ provider === 'deepseek' || provider === 'typesafe' ? '服务根地址，自动追加协议路径' : provider === 'mimo' ? 'OpenAI API 前缀，自动追加协议路径' : 'API 前缀' }}</span>
      </template>
      <ZInput v-model="form.base_url" type="url" placeholder="https://api.example.com/v1" :disabled="disabled" autocomplete="off" />
    </ZFormItem>
    <ZFormItem label="API Key" :required="!editing">
      <ZInput v-model="form.apiKey" type="password" :placeholder="editing ? '留空保留当前密钥' : '输入上游密钥'" :disabled="disabled" autocomplete="new-password" />
    </ZFormItem>
    <ZFormItem v-if="provider !== 'deepseek' && provider !== 'mimo' && provider !== 'typesafe'" label="传输方式">
      <ZSegmented v-model="form.transport" aria-label="传输方式" :options="transportOptions" :disabled="disabled" title="WS 优先需要上游支持 Responses WebSocket" />
    </ZFormItem>
    <ZFormItem v-if="provider === 'deepseek' || provider === 'mimo'" label="可用协议">
      <div class="flex flex-wrap gap-3">
        <ZCheckbox v-for="protocol in protocolOptions" :key="protocol.value" :label="protocol.label" :model-value="form.protocols?.includes(protocol.value) ?? false" :disabled="disabled" @update:model-value="toggleProtocol(protocol.value, $event)" />
      </div>
    </ZFormItem>
    <ZFormItem v-if="provider === 'typesafe'" label="协议">
      <span class="text-cp-sm text-cp-text-secondary">SystemOne · Noul / Choice / Score</span>
    </ZFormItem>
    <DeepSeekPricingFields v-if="provider === 'deepseek'" v-model="form.pricing_schedule" :disabled="disabled" />
  </div>
</template>
