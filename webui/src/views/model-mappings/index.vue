<script setup lang="ts">
import type { ModelMappingPolicy } from '@/api/modules/model-mappings'
import { defineTableColumns, ZButton, ZCard, ZConfirmDialog, ZIconButton, ZInput, ZMenuItem, ZPageHeader, ZPagination, ZPopover, ZSelect, ZTable } from '@codex-proxy/ui'
import { MoreHorizontal, Plus, Power, RefreshCw, Search, Trash2 } from '@lucide/vue'
import { computed } from 'vue'
import { formatProviderLabel } from '@/utils/providers'
import ModelMappingEditor from './components/ModelMappingEditor.vue'
import { useModelMappings } from './composables/useModelMappings'
import { mappingProtocolLabel } from './presenter'

const {
  search,
  status,
  groupOptions,
  catalogLoading,
  catalogError,
  showEditor,
  editing,
  draft,
  dirty,
  rowErrors,
  saveError,
  pending,
  saving,
  mutating,
  rows,
  loading,
  error,
  pagination,
  openEditor,
  save,
  mutate,
  refresh,
  pageChange,
  pageSizeChange,
} = useModelMappings()
const confirmation = computed({ get: () => Boolean(pending.value), set: (open) => {
  if (!open)
    pending.value = null
} })
const columns = defineTableColumns<ModelMappingPolicy>([
  { key: 'groupName', label: '所属分组', kind: 'identity', minWidth: 200 },
  { key: 'mappings', label: '映射概况', kind: 'custom', minWidth: 280 },
  { key: 'providers', label: '目标平台', kind: 'custom', width: 140 },
  { key: 'enabled', label: '状态', kind: 'status', width: 100 },
  { key: 'updatedAtDisplay', label: '更新时间', kind: 'datetime', width: 164 },
  { key: 'actions', label: '操作', kind: 'actions', width: 112 },
])
</script>

<template>
  <div class="flex min-h-0 w-full flex-col">
    <ZPageHeader class="h-17" title="模型映射" description="为分组配置模型路由" />
    <ZCard class="mt-5 flex flex-col">
      <template #header>
        <div class="flex w-full flex-col gap-3 sm:flex-row sm:flex-wrap sm:items-center">
          <ZInput v-model="search" class="sm:w-64" aria-label="搜索映射策略" placeholder="搜索分组名称">
            <template #prefix>
              <Search class="size-4.5 text-cp-text-tertiary" />
            </template>
          </ZInput>
          <ZSelect v-model="status" class="w-40" aria-label="按策略状态筛选" :options="[{ label: '全部状态', value: '' }, { label: '已启用', value: 'enabled' }, { label: '已停用', value: 'disabled' }]" />
          <div class="flex items-center justify-end gap-2 sm:ml-auto">
            <ZButton aria-label="刷新策略" :disabled="loading" @click="refresh">
              <RefreshCw class="size-4" />
            </ZButton>
            <ZButton type="primary" @click="openEditor()">
              <template #icon>
                <Plus class="size-4" />
              </template>添加策略
            </ZButton>
          </div>
        </div>
      </template>
      <template #body>
        <div class="flex min-h-0 flex-col">
          <p v-if="error" role="alert" class="mt-0 text-cp-sm text-cp-error-text">
            {{ error }}
          </p>
          <ZTable class="min-h-0" :columns="columns" :data="rows" :loading="loading" :empty-text="error ? '策略加载失败，请刷新重试' : '暂无模型映射策略'">
            <template #groupName="{ row }">
              <div class="grid min-w-0 gap-1.5">
                <strong class="truncate text-cp-sm text-cp-text" :title="row.groupName">{{ row.groupName }}</strong>
                <span v-if="row.remark" class="truncate text-cp-xs text-cp-text-tertiary" :title="row.remark">{{ row.remark }}</span>
              </div>
            </template>
            <template #mappings="{ row }">
              <div class="grid min-w-0 gap-1.5 py-2">
                <span class="text-cp-sm font-medium text-cp-text">{{ row.mappings.length }} 条映射</span>
                <span class="text-cp-xs leading-relaxed text-cp-text-secondary">{{ [...new Set(row.mappings.map(entry => mappingProtocolLabel(entry.protocol)))].join(' · ') }}</span>
              </div>
            </template>
            <template #providers="{ row }">
              <span class="text-cp-sm text-cp-text-secondary">{{ [...new Set(row.mappings.map(entry => formatProviderLabel(entry.targetProvider)))].join('、') }}</span>
            </template>
            <template #enabled="{ row }">
              <span class="inline-flex whitespace-nowrap rounded-lg px-2 py-1 text-cp-xs font-medium" :class="row.enabled ? 'bg-cp-success-container text-cp-success-on-container' : 'bg-cp-fill-tertiary text-cp-text-tertiary'">{{ row.enabled ? '已启用' : '已停用' }}</span>
            </template>
            <template #actions="{ row }">
              <div class="flex items-center gap-1">
                <ZButton size="small" variant="text" aria-label="编辑策略" :disabled="mutating" @click="openEditor(row)">
                  编辑
                </ZButton>
                <ZPopover placement="bottom-end">
                  <template #reference="{ open }">
                    <ZIconButton size="small" aria-label="更多策略操作" :pressed="open" :disabled="mutating">
                      <MoreHorizontal class="size-4" />
                    </ZIconButton>
                  </template>
                  <template #default="{ close }">
                    <div class="w-36 p-1.5">
                      <ZMenuItem :disabled="mutating" @click="close(); row.enabled ? pending = { policy: row, action: 'disable' } : mutate(row, 'enable')">
                        <template #icon>
                          <Power class="size-3.5" />
                        </template>
                        {{ row.enabled ? '停用策略' : '启用策略' }}
                      </ZMenuItem>
                      <ZMenuItem type="danger" :disabled="mutating" @click="close(); pending = { policy: row, action: 'delete' }">
                        <template #icon>
                          <Trash2 class="size-3.5" />
                        </template>
                        删除策略
                      </ZMenuItem>
                    </div>
                  </template>
                </ZPopover>
              </div>
            </template>
          </ZTable>
          <ZPagination v-bind="pagination" :disabled="loading" @current-change="pageChange" @size-change="pageSizeChange" />
        </div>
      </template>
    </ZCard>
    <ModelMappingEditor v-model="showEditor" v-model:draft="draft" :policy="editing" :group-options="groupOptions" :saving="saving" :loading="catalogLoading" :dirty="dirty" :errors="rowErrors" :error="saveError || catalogError" @save="save" />
    <ZConfirmDialog v-model="confirmation" :title="pending?.action === 'delete' ? '删除模型映射策略' : '停用模型映射策略'" :description="`操作后，${pending?.policy.groupName || '该分组'}的新请求将恢复原有路由规则`" :destructive="pending?.action === 'delete'" confirm-text="确认" :loading="mutating" @confirm="pending && mutate(pending.policy, pending.action)" />
  </div>
</template>
