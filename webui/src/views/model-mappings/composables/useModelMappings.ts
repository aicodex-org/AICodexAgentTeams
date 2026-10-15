import type { AccountGroup } from '@/api'
import type { ModelMappingDraft, ModelMappingPolicy } from '@/api/modules/model-mappings'
import { ZNotification } from '@codex-proxy/ui'
import { computed, onMounted, ref, shallowRef, watch } from 'vue'
import { getAccountGroups } from '@/api'
import { changeModelMappingPolicy, createModelMappingPolicy, getModelMappingPolicies, updateModelMappingPolicy } from '@/api/modules/model-mappings'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { usePagedQuery } from '@/composables/usePagedQuery'
import { useRequestState } from '@/composables/useRequestState'
import { errorMessage } from '@/utils/operation'
import { mappingValidationErrors } from '../presenter'

export function useModelMappings() {
  const search = shallowRef('')
  const status = shallowRef('')
  const groups = shallowRef<AccountGroup[]>([])
  const configuredGroupIds = shallowRef(new Set<string>())
  const catalog = useRequestState()
  const showEditor = shallowRef(false)
  const editing = shallowRef<ModelMappingPolicy | null>(null)
  const draft = ref<ModelMappingDraft>(emptyDraft())
  const savedDraft = shallowRef('')
  const saveError = shallowRef('')
  const pending = shallowRef<{ policy: ModelMappingPolicy, action: 'disable' | 'delete' } | null>(null)
  const savingAction = useAsyncAction()
  const mutationAction = useAsyncAction()
  const query = usePagedQuery({
    initialPageSize: 20,
    load: (pagination, options) => getModelMappingPolicies({ ...pagination, search: search.value.trim() || undefined, enabled: status.value ? status.value === 'enabled' : undefined }, options),
  })
  const pagination = computed(() => ({ currentPage: query.page.value, pageSize: query.pageSize.value, total: query.total.value }))
  const dirty = computed(() => JSON.stringify(draft.value) !== savedDraft.value)
  const rowErrors = computed(() => mappingValidationErrors(draft.value))
  const groupOptions = computed(() => groups.value.map(group => ({
    label: `${group.name}${configuredGroupIds.value.has(group.id) && group.id !== editing.value?.groupId ? '（已有策略）' : ''}`,
    value: group.id,
    disabled: configuredGroupIds.value.has(group.id) && group.id !== editing.value?.groupId,
  })))

  async function loadCatalog() {
    const id = catalog.start()
    const options = { signal: catalog.signal }
    try {
      const [firstGroups, firstPolicies] = await Promise.all([
        getAccountGroups({ page: 1, pageSize: 200 }, options),
        getModelMappingPolicies({ page: 1, pageSize: 200 }, options),
      ])
      const allGroups = [...firstGroups.items]
      const allPolicies = [...firstPolicies.items]
      for (let page = 2; page <= firstGroups.page.totalPages; page++) {
        const result = await getAccountGroups({ page, pageSize: firstGroups.page.pageSize }, options)
        allGroups.push(...result.items)
      }
      for (let page = 2; page <= firstPolicies.page.totalPages; page++) {
        const result = await getModelMappingPolicies({ page, pageSize: firstPolicies.page.pageSize }, options)
        allPolicies.push(...result.items)
      }
      if (!catalog.isCurrent(id))
        return
      groups.value = allGroups
      configuredGroupIds.value = new Set(allPolicies.map(policy => policy.groupId))
    }
    catch (cause) {
      catalog.fail(id, cause)
    }
    finally {
      catalog.finish(id)
    }
  }

  function openEditor(policy: ModelMappingPolicy | null = null) {
    editing.value = policy
    draft.value = policy ? { groupId: policy.groupId, enabled: policy.enabled, remark: policy.remark ?? '', mappings: policy.mappings.map(entry => ({ ...entry })) } : emptyDraft()
    savedDraft.value = JSON.stringify(draft.value)
    saveError.value = ''
    showEditor.value = true
    void loadCatalog()
  }

  async function save() {
    if (!draft.value.groupId || draft.value.mappings.length === 0 || rowErrors.value.some(Boolean)) {
      saveError.value = '请选择分组并填写完整、有效的映射条目'
      return
    }
    saveError.value = ''
    await savingAction.run(async () => {
      const data = { enabled: draft.value.enabled, remark: draft.value.remark.trim(), mappings: draft.value.mappings.map(entry => ({ ...entry })) }
      if (editing.value)
        await updateModelMappingPolicy({ ...data, id: editing.value.id, expectedRevision: editing.value.revision })
      else
        await createModelMappingPolicy({ ...data, groupId: draft.value.groupId })
      showEditor.value = false
      ZNotification.success({ message: '模型映射策略已保存' })
      await refresh()
    }, { onError: cause => saveError.value = errorMessage(cause) })
  }

  async function mutate(policy: ModelMappingPolicy, action: 'enable' | 'disable' | 'delete') {
    await mutationAction.run(async () => {
      await changeModelMappingPolicy(action, { id: policy.id, expectedRevision: policy.revision })
      pending.value = null
      ZNotification.success({ message: action === 'delete' ? '策略已删除' : action === 'enable' ? '策略已启用' : '策略已停用' })
      await refresh()
    })
  }

  async function refresh() {
    await Promise.all([query.execute(), loadCatalog()])
  }

  function pageChange(page: number) {
    query.page.value = page
    void query.execute()
  }

  function pageSizeChange(size: number) {
    query.pageSize.value = size
    pageChange(1)
  }

  watch(search, (_value, _old, cleanup) => {
    const timer = setTimeout(pageChange, 250, 1)
    cleanup(() => clearTimeout(timer))
  })
  watch(status, () => pageChange(1))
  onMounted(refresh)
  return { search, status, groups, groupOptions, catalogLoading: catalog.loading, catalogError: catalog.error, showEditor, editing, draft, dirty, rowErrors, saveError, pending, saving: savingAction.loading, mutating: mutationAction.loading, rows: query.items, loading: query.loading, error: query.error, pagination, openEditor, save, mutate, refresh, pageChange, pageSizeChange }
}

function emptyDraft(): ModelMappingDraft {
  return { groupId: '', enabled: true, remark: '', mappings: [{ protocol: 'openai-chat', requestedModel: '', routeModel: '', targetProvider: '' }] }
}
