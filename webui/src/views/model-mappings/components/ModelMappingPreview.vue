<script setup lang="ts">
import type { ApiKey } from '@/api'
import type { ModelMappingDraft, ModelMappingPolicy } from '@/api/modules/model-mappings'
import { ZButton, ZDialog, ZSelect } from '@codex-proxy/ui'
import { computed, shallowRef, watch } from 'vue'
import { getApiKeys } from '@/api'
import { previewModelMapping } from '@/api/modules/model-mappings'
import { useRequestState } from '@/composables/useRequestState'
import { formatProviderLabel } from '@/utils/providers'
import { mappingProtocolLabel } from '../presenter'

const props = defineProps<{ draft: ModelMappingDraft, policy: ModelMappingPolicy | null, valid: boolean }>()
const expanded = shallowRef(false)
const keys = shallowRef<ApiKey[]>([])
const keyId = shallowRef('')
const entryIndex = shallowRef(0)
const selectedEntry = computed(() => props.draft.mappings[entryIndex.value])
watch(() => props.draft.mappings.length, (count) => {
  entryIndex.value = Math.max(0, Math.min(entryIndex.value, count - 1))
}, { flush: 'sync' })
const keysRequest = useRequestState()
const previewRequest = useRequestState()
const result = shallowRef<Awaited<ReturnType<typeof previewModelMapping>>>()
const loading = previewRequest.loading
const error = previewRequest.error
const keysError = keysRequest.error
const options = computed(() => keys.value.map(key => ({ label: key.name || key.prefix, value: key.id })))
const entries = computed(() => props.draft.mappings.map((entry, value) => ({ label: `${mappingProtocolLabel(entry.protocol)} · ${entry.requestedModel}`, value })))
watch(() => [props.draft, keyId.value, entryIndex.value], () => {
  previewRequest.invalidate()
  previewRequest.error.value = ''
  result.value = undefined
}, { deep: true })
watch(expanded, async (open) => {
  if (!open)
    return
  const id = keysRequest.start()
  try {
    const items: ApiKey[] = []
    let cursor: string | undefined
    do {
      const page = await getApiKeys({ limit: 200, cursor }, { signal: keysRequest.signal })
      items.push(...page.items)
      cursor = page.nextCursor ?? undefined
    } while (cursor && keysRequest.isCurrent(id))
    if (keysRequest.isCurrent(id))
      keys.value = items
  }
  catch (cause) { keysRequest.fail(id, cause) }
  finally { keysRequest.finish(id) }
})
async function preview() {
  const entry = selectedEntry.value
  if (!entry || !props.valid || !keyId.value)
    return
  result.value = undefined
  const id = previewRequest.start()
  try {
    const value = await previewModelMapping({ groupId: props.draft.groupId, policyId: props.policy?.id, revision: props.policy?.revision, enabled: props.draft.enabled, mappings: props.draft.mappings, clientKeyId: keyId.value, protocol: entry.protocol, requestedModel: entry.requestedModel }, { signal: previewRequest.signal })
    if (previewRequest.isCurrent(id))
      result.value = value
  }
  catch (cause) { previewRequest.fail(id, cause) }
  finally { previewRequest.finish(id) }
}
</script>

<template>
  <ZButton variant="text" aria-haspopup="dialog" @click="expanded = true">
    预览路由
  </ZButton>
  <ZDialog v-model="expanded" title="路由预览" width="48rem">
    <div class="grid min-w-0 gap-4">
      <div class="grid min-w-0 gap-3 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto]">
        <ZSelect v-model="keyId" :options="options" aria-label="预览 API 密钥" placeholder="选择 API 密钥" />
        <ZSelect v-model="entryIndex" :options="entries" aria-label="预览映射条目" />
        <ZButton :loading="loading" :disabled="!valid || !keyId || !selectedEntry" @click="preview">
          检查路由
        </ZButton>
      </div>
      <p class="m-0 text-cp-xs text-cp-text-tertiary">
        按当前草稿检查密钥权限与映射冲突，不发送上游请求
      </p>
      <p v-if="error || keysError" role="alert" class="m-0 text-cp-sm text-cp-error-text">
        {{ error || keysError }}
      </p>
      <div v-if="result" role="status" class="grid gap-2 text-cp-sm">
        <span>{{ result.modelMapping ? '命中模型映射' : '未命中映射，使用原有路由' }}</span>
        <span v-for="target in result.targets" :key="`${target.provider}:${target.routeModel}`">{{ formatProviderLabel(target.provider) }} · <code>{{ target.routeModel }}</code></span>
      </div>
    </div>
    <template #footer>
      <ZButton @click="expanded = false">
        返回编辑
      </ZButton>
    </template>
  </ZDialog>
</template>
