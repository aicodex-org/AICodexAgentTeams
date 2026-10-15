import type { Ref } from 'vue'
import type { MappingField } from '../presenter'
import type { MappingProtocol, ModelMappingDraft, ModelMappingEntry } from '@/api/modules/model-mappings'
import { computed, nextTick, shallowRef, watch } from 'vue'
import { formatProviderLabel } from '@/utils/providers'
import { mappingProtocolLabel } from '../presenter'

// 行标识只属于编辑草稿，筛选和分页不能改变实际保存的条目顺序
export function useMappingRows(draft: Ref<ModelMappingDraft>) {
  const search = shallowRef('')
  const protocolFilter = shallowRef<MappingProtocol | ''>('')
  const providerFilter = shallowRef('')
  const page = shallowRef(1)
  const pageSize = shallowRef(5)
  const identities = new WeakMap<ModelMappingEntry, string>()
  const inputs = new Map<string, { focus: () => void }>()
  let sequence = 0
  const allRows = computed(() => draft.value.mappings.map((entry, index) => {
    let id = identities.get(entry)
    if (!id) {
      id = `mapping-row-${++sequence}`
      identities.set(entry, id)
    }
    return { id, entry, index }
  }))
  const matchedIds = shallowRef<Set<string> | null>(null)
  // 查找条件变化时确定结果，编辑命中项时不因文字变化卸载输入框
  watch([search, protocolFilter, providerFilter], ([value, protocol, provider]) => {
    page.value = 1
    const query = value.trim().toLowerCase()
    matchedIds.value = query || protocol || provider
      ? new Set(allRows.value.filter(({ entry }) => (!protocol || entry.protocol === protocol)
        && (!provider || entry.targetProvider === provider)
        && (!query || [entry.requestedModel, entry.routeModel, entry.targetProvider, formatProviderLabel(entry.targetProvider), entry.protocol, mappingProtocolLabel(entry.protocol)]
          .some(text => text.toLowerCase().includes(query)))).map(row => row.id))
      : null
  }, { flush: 'sync' })
  const filteredRows = computed(() => matchedIds.value
    ? allRows.value.filter(row => matchedIds.value?.has(row.id))
    : allRows.value)
  const pageRows = computed(() => filteredRows.value.slice((page.value - 1) * pageSize.value, page.value * pageSize.value))
  watch(() => draft.value, () => {
    clearFilters()
    page.value = 1
  })
  watch(() => filteredRows.value.length, (total) => {
    page.value = Math.max(1, Math.min(page.value, Math.ceil(total / pageSize.value)))
  }, { flush: 'sync' })
  watch(pageSize, (size, previousSize) => {
    const firstIndex = (page.value - 1) * previousSize
    page.value = Math.max(1, Math.min(Math.floor(firstIndex / size) + 1, Math.ceil(filteredRows.value.length / size)))
  }, { flush: 'sync' })

  function setInput(id: string, field: MappingField, input: unknown) {
    const key = `${id}:${field}`
    if (input && typeof input === 'object' && 'focus' in input && typeof input.focus === 'function')
      inputs.set(key, input as { focus: () => void })
    else
      inputs.delete(key)
  }

  async function locate(index: number, field: MappingField = 'requestedModel') {
    clearFilters()
    page.value = Math.floor(index / pageSize.value) + 1
    const row = allRows.value[index]
    await nextTick()
    if (row)
      inputs.get(`${row.id}:${field}`)?.focus()
  }

  async function add() {
    draft.value.mappings.push({ protocol: protocolFilter.value || 'openai-chat', requestedModel: '', routeModel: '', targetProvider: providerFilter.value })
    await locate(draft.value.mappings.length - 1)
  }

  function remove(id: string) {
    const row = allRows.value.find(item => item.id === id)
    if (row)
      draft.value.mappings.splice(row.index, 1)
  }

  function clearFilters() {
    search.value = ''
    protocolFilter.value = ''
    providerFilter.value = ''
  }

  return { search, protocolFilter, providerFilter, page, pageSize, filteredRows, pageRows, setInput, locate, add, remove, clearFilters }
}
