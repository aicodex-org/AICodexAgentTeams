<script setup lang="ts">
import type { MappingValidationIssue } from '../presenter'
import type { ModelMappingDraft, ModelMappingEntry, ModelMappingPolicy, ModelMappingTarget } from '@/api/modules/model-mappings'
import { ZButton, ZConfirmDialog, ZDialog, ZForm, ZFormItem, ZIconButton, ZInput, ZPagination, ZSelect, ZSwitch } from '@codex-proxy/ui'
import { ArrowRight, Plus, Search, Trash2 } from '@lucide/vue'
import { useMediaQuery } from '@vueuse/core'
import { computed, shallowRef, watch } from 'vue'
import { getModelMappingTargets } from '@/api/modules/model-mappings'
import { useRequestState } from '@/composables/useRequestState'
import { formatProviderLabel } from '@/utils/providers'
import { useMappingRows } from '../composables/useMappingRows'
import { mappingProtocolOptions, mappingValidationIssues } from '../presenter'
import ModelMappingPreview from './ModelMappingPreview.vue'

const props = defineProps<{
  policy: ModelMappingPolicy | null
  groupOptions: { label: string, value: string, disabled: boolean }[]
  saving: boolean
  loading: boolean
  dirty: boolean
  errors: string[]
  error: string
}>()
const emit = defineEmits<{ save: [] }>()
const open = defineModel<boolean>({ required: true })
const draft = defineModel<ModelMappingDraft>('draft', { required: true })
const discard = shallowRef(false)
const { search, protocolFilter, providerFilter, page, pageSize, filteredRows, pageRows, setInput, locate, add, remove, clearFilters } = useMappingRows(draft)
const mobile = useMediaQuery('(max-width: 767px)')
const validationAttempted = shallowRef(false)
const activeErrorIndex = shallowRef(-1)
const targets = shallowRef<ModelMappingTarget[]>([])
const targetRequest = useRequestState()
const targetLoading = targetRequest.loading
const targetError = targetRequest.error
function targetSupported(entry: ModelMappingEntry) {
  return targets.value.some(target => target.protocol === entry.protocol && target.provider === entry.targetProvider)
}
function providerOptions(entry: ModelMappingEntry) {
  const options = targets.value.filter(target => target.protocol === entry.protocol).map(target => ({ label: formatProviderLabel(target.provider), value: target.provider, disabled: false }))
  if (entry.targetProvider && !options.some(option => option.value === entry.targetProvider))
    options.push({ label: `${formatProviderLabel(entry.targetProvider)}${targetLoading.value || targetError.value ? '' : '（协议不支持）'}`, value: entry.targetProvider, disabled: true })
  return options
}
const targetsValid = computed(() => draft.value.mappings.every(targetSupported))
const filtering = computed(() => Boolean(search.value.trim() || protocolFilter.value || providerFilter.value))
const protocolFilters = [{ label: '全部协议', value: '' }, ...mappingProtocolOptions]
const providerFilters = computed(() => [{ label: '全部平台', value: '' }, ...Array.from(new Set([
  ...targets.value.map(target => target.provider),
  ...draft.value.mappings.map(entry => entry.targetProvider).filter(Boolean),
])).map(provider => ({ label: formatProviderLabel(provider), value: provider }))])
const validationIssues = computed(() => mappingValidationIssues(draft.value))
function rowIssue(entry: ModelMappingEntry, index: number): MappingValidationIssue | null {
  const issue = validationIssues.value[index]
  if (issue)
    return issue
  if (draft.value.groupId && !targetLoading.value && !targetError.value && !targetSupported(entry))
    return { field: 'targetProvider', message: '目标平台不支持当前协议，请重新选择' }
  return null
}
function rowError(entry: ModelMappingEntry, index: number) {
  return rowIssue(entry, index)?.message ?? ''
}
async function loadTargets() {
  const groupId = draft.value.groupId
  targets.value = []
  if (!groupId || !open.value) {
    targetRequest.invalidate()
    return
  }
  const id = targetRequest.start()
  try {
    const result = await getModelMappingTargets(groupId, { signal: targetRequest.signal })
    if (targetRequest.isCurrent(id))
      targets.value = result.targets
  }
  catch (cause) { targetRequest.fail(id, cause) }
  finally { targetRequest.finish(id) }
}
watch([() => draft.value.groupId, open], loadTargets)
const dialogOpen = computed({ get: () => open.value, set: (value) => {
  if (!value && props.dirty)
    discard.value = true
  else
    open.value = value
} })

const invalidRows = computed(() => draft.value.mappings.flatMap((entry, index) => {
  const issue = rowIssue(entry, index)
  return issue ? [{ index, field: issue.field }] : []
}))
async function locateError(next = false) {
  const error = (next && invalidRows.value.find(row => row.index > activeErrorIndex.value)) || invalidRows.value[0]
  if (!error)
    return
  activeErrorIndex.value = error.index
  await locate(error.index, error.field)
}
async function submit() {
  if (targetLoading.value || targetError.value)
    return
  if (invalidRows.value.length) {
    validationAttempted.value = true
    await locateError()
    return
  }
  validationAttempted.value = false
  emit('save')
}
watch(() => draft.value, () => {
  validationAttempted.value = false
  activeErrorIndex.value = -1
})
watch(() => invalidRows.value.length, (count) => {
  if (!count)
    validationAttempted.value = false
})
</script>

<template>
  <ZDialog v-model="dialogOpen" :title="policy ? '编辑模型映射' : '添加模型映射策略'" :width="1120" :fullscreen="mobile" :draggable="!mobile" :show-close="!saving" :close-on-click-modal="!saving" :close-on-press-escape="!saving">
    <ZForm class="mapping-editor grid min-w-0 gap-4 border-t border-cp-border-secondary pt-4" :class="{ 'mapping-editor--feedback': (validationAttempted && invalidRows.length) || targetError || error }">
      <div class="grid min-w-0 grid-cols-[minmax(0,1fr)_auto] items-end gap-x-5 gap-y-3 md:grid-cols-[minmax(0,1fr)_minmax(0,1.5fr)_auto]">
        <ZFormItem class="min-w-0" label="所属分组" required>
          <div v-if="policy" class="flex h-cp-control min-w-0 items-center text-cp-sm font-semibold text-cp-text" :title="policy.groupName">
            <span class="truncate">{{ policy.groupName }}</span>
          </div>
          <ZSelect v-else v-model="draft.groupId" class="w-full" :options="groupOptions" placeholder="选择分组" aria-label="所属分组" :disabled="saving || loading" />
        </ZFormItem>
        <ZFormItem class="col-span-2 min-w-0 md:col-span-1 md:col-start-2 md:row-start-1" label="备注">
          <ZInput v-model="draft.remark" class="mapping-model-input" aria-label="策略备注" placeholder="选填" :disabled="saving" />
        </ZFormItem>
        <div class="col-start-2 row-start-1 flex h-cp-control items-center gap-2.5 md:col-start-3">
          <span class="text-cp-sm text-cp-text-secondary">启用</span>
          <ZSwitch v-model="draft.enabled" class="min-h-11 md:min-h-0" aria-label="启用策略" :disabled="saving" />
        </div>
      </div>
      <section class="grid min-w-0 gap-3" aria-label="映射条目">
        <div class="flex items-center justify-between gap-3 text-cp-xs text-cp-text-tertiary">
          <span>仅在当前分组、指定协议内生效</span>
          <span class="shrink-0">{{ filtering ? `匹配 ${filteredRows.length} / ${draft.mappings.length} 条` : `共 ${draft.mappings.length} 条` }}</span>
        </div>
        <div class="grid min-w-0 grid-cols-2 gap-2 md:grid-cols-[minmax(0,1fr)_150px_140px_auto]">
          <ZInput v-model="search" class="col-span-2 md:col-span-1" aria-label="查找映射条目" placeholder="查找模型 ID、平台或协议" clearable :disabled="saving">
            <template #prefix>
              <Search class="size-3.5 text-cp-text-tertiary" />
            </template>
          </ZInput>
          <ZSelect v-model="protocolFilter" :options="protocolFilters" aria-label="筛选请求协议" :disabled="saving" />
          <ZSelect v-model="providerFilter" :options="providerFilters" aria-label="筛选目标平台" :disabled="saving" />
          <ZButton class="col-span-2 md:col-span-1" :disabled="saving" @click="add">
            <template #icon>
              <Plus class="size-4" />
            </template>添加映射
          </ZButton>
        </div>
        <div class="overflow-hidden rounded-cp border border-cp-border-secondary">
          <div class="mapping-entry-grid mapping-table-heading items-center bg-cp-fill-quaternary px-3 py-2.5 text-cp-xs text-cp-text-secondary" aria-hidden="true">
            <span>#</span><span>请求协议</span><span>请求模型 ID</span><span /><span>路由模型 ID</span><span>目标平台</span><span />
          </div>
          <div class="mapping-rows min-w-0 overscroll-contain" :class="{ 'mapping-rows--paged': pageSize > 5 }">
            <div v-for="row in pageRows" :key="row.id" class="mapping-row min-w-0 border-b border-cp-border-secondary px-3 py-2.5 last:border-b-0" :data-mapping-row="row.id">
              <div class="mapping-entry-grid items-center">
                <span class="mapping-number text-cp-xs tabular-nums text-cp-text-quaternary">{{ row.index + 1 }}</span>
                <ZSelect :ref="input => setInput(row.id, 'protocol', input)" v-model="row.entry.protocol" :aria-invalid="rowIssue(row.entry, row.index)?.field === 'protocol' || undefined" :aria-describedby="rowIssue(row.entry, row.index)?.field === 'protocol' ? `${row.id}-error` : undefined" size="small" class="mapping-protocol min-w-0" :options="mappingProtocolOptions" :aria-label="`第 ${row.index + 1} 条请求协议`" :disabled="saving" />
                <div class="mapping-model-pair min-w-0">
                  <div class="mapping-model-field grid min-w-0 gap-1.5">
                    <!-- ZInput 将 id 传给原生 input，保持可见标签关联 -->
                    <!-- eslint-disable-next-line vue-a11y/label-has-for -->
                    <label :for="`${row.id}-request`" class="text-cp-xs text-cp-text-secondary md:hidden">请求模型 ID</label>
                    <ZInput :id="`${row.id}-request`" :ref="input => setInput(row.id, 'requestedModel', input)" v-model="row.entry.requestedModel" :aria-invalid="rowIssue(row.entry, row.index)?.field === 'requestedModel' || undefined" :aria-describedby="rowIssue(row.entry, row.index)?.field === 'requestedModel' ? `${row.id}-error` : undefined" size="small" class="mapping-model-input min-w-0" :aria-label="`第 ${row.index + 1} 条请求模型 ID`" placeholder="请求模型 ID" :disabled="saving" />
                  </div>
                  <ArrowRight class="size-4 text-cp-text-quaternary" aria-hidden="true" />
                  <div class="mapping-model-field grid min-w-0 gap-1.5">
                    <!-- ZInput 将 id 传给原生 input，保持可见标签关联 -->
                    <!-- eslint-disable-next-line vue-a11y/label-has-for -->
                    <label :for="`${row.id}-route`" class="text-cp-xs text-cp-text-secondary md:hidden">路由模型 ID</label>
                    <ZInput :id="`${row.id}-route`" :ref="input => setInput(row.id, 'routeModel', input)" v-model="row.entry.routeModel" :aria-invalid="rowIssue(row.entry, row.index)?.field === 'routeModel' || undefined" :aria-describedby="rowIssue(row.entry, row.index)?.field === 'routeModel' ? `${row.id}-error` : undefined" size="small" class="mapping-model-input min-w-0" :aria-label="`第 ${row.index + 1} 条路由模型 ID`" placeholder="路由模型 ID" :disabled="saving" />
                  </div>
                </div>
                <ZSelect :ref="input => setInput(row.id, 'targetProvider', input)" v-model="row.entry.targetProvider" :aria-invalid="rowIssue(row.entry, row.index)?.field === 'targetProvider' || undefined" :aria-describedby="rowIssue(row.entry, row.index)?.field === 'targetProvider' ? `${row.id}-error` : undefined" size="small" class="mapping-provider min-w-0" :options="providerOptions(row.entry)" placeholder="目标平台" :aria-label="`第 ${row.index + 1} 条目标平台`" :disabled="saving || !draft.groupId || loading || targetLoading" />
                <ZIconButton size="small" class="mapping-remove" :disabled="saving" :aria-label="`删除第 ${row.index + 1} 条映射`" @click="remove(row.id)">
                  <Trash2 class="size-3.5 text-cp-error" />
                </ZIconButton>
              </div>
              <p v-if="rowError(row.entry, row.index)" :id="`${row.id}-error`" role="status" class="mb-0 mt-2 text-cp-xs text-cp-error-text">
                {{ rowError(row.entry, row.index) }}
              </p>
            </div>
            <div v-if="!pageRows.length" class="grid justify-items-center gap-2 py-8 text-cp-sm text-cp-text-tertiary">
              <span>{{ filtering ? '没有匹配的映射' : '尚未添加映射' }}</span>
              <ZButton v-if="filtering" size="small" variant="text" @click="clearFilters">
                清除查找
              </ZButton>
            </div>
          </div>
        </div>
      </section>
      <div v-if="validationAttempted && invalidRows.length" role="alert" class="flex items-center justify-between gap-3 rounded-cp bg-cp-error-container px-3 py-2 text-cp-xs text-cp-error-text">
        <span>共 {{ invalidRows.length }} 条映射需要修正</span>
        <ZButton size="small" variant="text" @click="locateError(true)">
          {{ invalidRows.length > 1 ? '下一处问题' : '定位问题' }}
        </ZButton>
      </div>
      <div v-if="targetError" role="alert" class="flex items-center justify-between gap-3 text-cp-sm text-cp-error-text">
        <span>目标平台加载失败：{{ targetError }}</span>
        <ZButton size="small" variant="text" @click="loadTargets">
          重试加载平台
        </ZButton>
      </div>
      <p v-if="error" role="alert" class="m-0 text-cp-sm text-cp-error-text">
        {{ error }}
      </p>
    </ZForm>
    <template #footer>
      <div class="mapping-footer grid w-full gap-3 border-t border-cp-border-secondary pt-3 md:grid-cols-[minmax(0,1fr)_auto] md:items-center md:gap-6">
        <div class="min-w-0" data-mapping-pagination>
          <ZPagination v-model:current-page="page" v-model:page-size="pageSize" :total="filteredRows.length" :page-sizes="[5, 10, 20]" :disabled="saving" />
        </div>
        <div class="flex flex-wrap items-center justify-end gap-2">
          <span v-if="dirty" class="mr-auto text-cp-xs text-cp-warning-text md:mr-1">未保存</span>
          <ModelMappingPreview v-if="open" :draft="draft" :policy="policy" :valid="targetsValid && !errors.some(Boolean) && Boolean(draft.groupId) && draft.mappings.length > 0" />
          <ZButton :disabled="saving" @click="dialogOpen = false">
            取消
          </ZButton>
          <ZButton type="primary" :loading="saving" :disabled="loading || targetLoading || Boolean(targetError) || !draft.groupId || !draft.mappings.length" @click="submit">
            保存策略
          </ZButton>
        </div>
      </div>
    </template>
  </ZDialog>
  <ZConfirmDialog v-model="discard" title="放弃未保存的修改" description="关闭后，本次修改不会保存" confirm-text="放弃修改" cancel-text="继续编辑" @confirm="discard = false; open = false" />
</template>

<style scoped>
.mapping-entry-grid {
  display: grid;
  grid-template-columns: 24px 150px minmax(0, 1fr) 16px minmax(0, 1fr) 140px 28px;
  gap: 8px;
}

.mapping-model-pair {
  display: contents;
}

.mapping-rows {
  max-height: none;
  overflow: visible;
}

.mapping-editor--feedback {
  --mapping-feedback-space: 4rem;
}

@media (min-width: 768px) and (min-height: 651px) {
  .mapping-rows--paged {
    /* 大分页为表单、工具栏、固定底栏及错误汇总留足空间 */
    max-height: min(50dvh, calc(100dvh - 29rem - var(--mapping-feedback-space, 0px)));
    overflow-y: auto;
  }
}

.mapping-model-input {
  --cp-input-bg: var(--cp-color-bg-container);
  --cp-input-hover-bg: var(--cp-color-bg-container);
}

.mapping-model-input :deep(.group\/input) {
  box-shadow: inset 0 0 0 1px var(--cp-color-border-secondary);
}

.mapping-model-input :deep(.group\/input:focus-within) {
  box-shadow:
    inset 0 0 0 1px var(--cp-color-primary),
    0 0 0 2px var(--cp-control-outline);
}

.mapping-model-pair :deep(input) {
  font-family: var(--font-mono, ui-monospace, monospace);
  font-size: 14px;
  font-weight: 400;
}

@media (max-width: 767px) {
  .mapping-editor,
  .mapping-footer {
    --cp-control-height: 44px;
    --cp-control-height-sm: 44px;
    --cp-control-height-lg: 44px;
  }

  .mapping-entry-grid {
    grid-template-columns: 20px minmax(0, 1fr) minmax(0, 1fr) 44px;
  }

  .mapping-footer :deep(button) {
    min-width: 44px;
    min-height: 44px;
  }

  [data-mapping-pagination] :deep(.w-28) {
    width: 6rem;
  }

  [data-mapping-pagination] :deep(nav) {
    gap: 4px;
  }

  .mapping-table-heading {
    display: none;
  }

  .mapping-model-pair {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    grid-column: 1 / -1;
    grid-row: 2;
    align-items: center;
    gap: 8px;
  }

  .mapping-provider {
    grid-column: 3;
    grid-row: 1;
  }

  .mapping-remove {
    grid-column: 4;
    grid-row: 1;
  }

  .mapping-model-pair > svg {
    justify-self: center;
    transform: rotate(90deg);
  }
}
</style>
