<script setup lang="ts">
import type { UsageModelMapping } from '@/api'
import { ZPopover } from '@codex-proxy/ui'
import { ArrowRightLeft } from '@lucide/vue'
import { formatProviderLabel } from '@/utils/providers'

withDefaults(defineProps<{ mapping: UsageModelMapping, expanded?: boolean }>(), { expanded: false })
const protocols: Record<string, string> = { 'openai': 'Responses', 'openai-chat': 'Chat Completions', 'anthropic': 'Messages' }
</script>

<template>
  <div v-if="expanded" class="grid gap-2 rounded-cp bg-cp-fill-quaternary p-3 text-cp-xs">
    <strong class="text-cp-text">{{ mapping.source === 'global' ? '全局模型映射' : mapping.source === 'extension' ? '插件模型映射' : '分组模型映射' }}</strong>
    <code class="break-all">{{ mapping.requestedModel }} → {{ mapping.routeModel }}</code>
    <span>{{ protocols[mapping.protocol] || mapping.protocol }} · {{ formatProviderLabel(mapping.targetProvider) }}</span>
    <span v-for="policy in mapping.policies" :key="policy.policyId" class="break-all text-cp-text-secondary">
      {{ policy.groupName }} · 策略 {{ policy.policyId }} · 版本 {{ policy.policyRevision }}
    </span>
  </div>
  <ZPopover v-else trigger="click" placement="bottom">
    <template #reference>
      <button type="button" aria-label="查看模型映射" class="inline-flex w-fit cursor-pointer items-center gap-1 rounded-md border-0 bg-cp-primary-container px-1.5 py-1 text-cp-xs text-cp-primary-on-container outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline">
        <ArrowRightLeft class="size-3" aria-hidden="true" />模型映射
      </button>
    </template>
    <div class="grid max-w-96 gap-2 p-3 text-cp-xs">
      <code class="break-all">{{ mapping.requestedModel }} → {{ mapping.routeModel }}</code>
      <span>{{ protocols[mapping.protocol] || mapping.protocol }} · {{ formatProviderLabel(mapping.targetProvider) }}</span>
      <span v-for="policy in mapping.policies" :key="policy.policyId" class="break-all text-cp-text-secondary">{{ policy.groupName }} · 版本 {{ policy.policyRevision }}</span>
    </div>
  </ZPopover>
</template>
