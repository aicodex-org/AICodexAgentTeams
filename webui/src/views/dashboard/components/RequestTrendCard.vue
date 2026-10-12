<script setup lang="ts">
import type { dashboardTrendView, normalizeDashboardTrendKind } from '../presenter'

import { ZButton, ZCard, ZEmpty, ZSegmented } from '@codex-proxy/ui'
import { RefreshCw } from '@lucide/vue'
import { toRef } from 'vue'
import BaseChart from '@/components/charts/BaseChart.vue'
import { useRequestTrendChart } from '../composables/useRequestTrendChart'

type TrendKind = ReturnType<typeof normalizeDashboardTrendKind>
type TrendView = ReturnType<typeof dashboardTrendView>

const props = withDefaults(defineProps<{
  points: TrendView['points']
  summary: TrendView['summary']
  loading?: boolean
  error?: string
}>(), {
  loading: false,
  error: '',
})

const emit = defineEmits<{
  trendChange: [kind: TrendKind]
}>()

const activeKind = defineModel<TrendKind>('kind', { required: true })
const {
  tabs,
  pinnedSummaryLabel,
  hasSamples,
  chartOption,
  summaryMarkerStyle,
  handleTrendChange,
  toggleSummarySeries,
  isSummarySeriesActive,
} = useRequestTrendChart({
  points: toRef(props, 'points'),
  summary: toRef(props, 'summary'),
  activeKind,
  onTrendChange: kind => emit('trendChange', kind),
})
</script>

<template>
  <ZCard as="article" title="使用趋势" class="w-full">
    <template #actions>
      <ZSegmented
        v-model="activeKind"
        aria-label="趋势指标"
        :options="tabs"
        class="w-full sm:w-48"
        @update:model-value="handleTrendChange"
      />
    </template>

    <template #body>
      <div class="grid gap-3.5">
        <div class="grid h-14.25 min-w-0 grid-cols-3 gap-1.5 rounded-xl bg-cp-fill-quaternary/45 p-1.5">
          <button
            v-for="item in props.summary"
            :key="item.label"
            type="button"
            :aria-label="`突出显示${item.label}曲线`"
            :aria-pressed="pinnedSummaryLabel === item.label"
            class="group grid min-w-0 grid-cols-[8px_minmax(0,1fr)] items-center gap-x-2 rounded-lg border-0 bg-transparent px-2.5 py-2 text-left outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline"
            @click="toggleSummarySeries(item.label)"
          >
            <i
              class="size-2 rounded-full transition-transform duration-200 ease-out group-hover:scale-125 motion-reduce:transition-none"
              :style="summaryMarkerStyle(item)"
              :class="[
                isSummarySeriesActive(item.label) ? 'scale-125' : undefined,
                {
                  'bg-cp-info': item.tone === 'info',
                  'bg-cp-success': item.tone === 'success',
                  'bg-cp-warning': item.tone === 'warning',
                  'bg-cp-error': item.tone === 'danger',
                  'bg-cp-text-tertiary': item.tone === 'normal',
                },
              ]"
            />
            <span class="grid min-w-0 gap-1.5">
              <span class="truncate text-[10px] leading-none font-bold text-cp-text-secondary">
                {{ item.label }}
              </span>
              <strong
                class="truncate font-mono text-cp-xl leading-none font-heavy tabular-nums text-cp-text"
                :title="item.value"
              >
                {{ item.value }}
              </strong>
            </span>
          </button>
        </div>

        <div class="relative h-55 w-full overflow-hidden" :aria-busy="loading">
          <BaseChart v-if="hasSamples" :option="chartOption" :height="220" />
          <div v-else-if="loading" role="status" class="flex h-full items-center justify-center gap-2 text-xs text-cp-text-secondary">
            <RefreshCw :size="16" class="animate-spin motion-reduce:animate-none" />
            正在加载趋势
          </div>
          <ZEmpty
            v-else
            surface="none"
            :title="error ? '趋势暂不可用' : '暂无趋势数据'"
            :description="error || '产生请求后，可在这里查看当日用量、延迟与错误趋势'"
            class="h-full place-content-center"
          >
            <template v-if="error" #action>
              <ZButton size="small" @click="emit('trendChange', activeKind)">
                重新加载
              </ZButton>
            </template>
          </ZEmpty>
        </div>
      </div>
    </template>
  </ZCard>
</template>
