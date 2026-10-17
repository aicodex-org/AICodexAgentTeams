<script setup lang="ts">
import type { Account, AccountStatistics, AccountStatisticsPeriod } from '@/api'

import { ZButton, ZDialog } from '@codex-proxy/ui'
import { RefreshCw } from '@lucide/vue'
import { computed, ref } from 'vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import { formatDuration, formatInteger, formatUsd } from '@/utils/format'
import AccountIdentityCell from './AccountIdentityCell.vue'

const props = defineProps<{
  account: Account
  statistics: AccountStatistics | null
  loading: boolean
  error: string
}>()
const emit = defineEmits<{ refresh: [] }>()
const open = defineModel<boolean>({ default: false })
const content = ref<HTMLElement | null>(null)

function refresh() {
  // 刷新会禁用按钮或移除错误区，先将焦点保留在弹窗内以维持键盘关闭能力
  content.value?.focus({ preventScroll: true })
  emit('refresh')
}
const averageFormatter = new Intl.NumberFormat('zh-CN', { maximumFractionDigits: 2 })

function costCoverage(period: AccountStatisticsPeriod) {
  const details: string[] = []
  if (period.partialCostCount > 0)
    details.push(`${formatInteger(period.partialCostCount)} 条费用不完整`)
  if (period.unknownCostCount > 0)
    details.push(`${formatInteger(period.unknownCostCount)} 条费用未知`)
  if (period.nonUsdCostPresent)
    details.push('非 USD 费用未计入')
  return details.join(' · ') || (period.requestCount === 0 ? '暂无请求费用' : '按请求费用快照汇总')
}

const cards = computed(() => {
  const data = props.statistics
  const averageDetail = data?.activeDays ? `按近 30 天内 ${data.activeDays} 个实际使用日计算` : '近 30 天内暂无使用日'
  return [
    { label: '近 7 天请求数', value: data ? formatInteger(data.sevenDays.requestCount) : '—', detail: data ? `${data.sevenDays.startDisplay} – ${data.sevenDays.endDisplay}` : '', cost: false },
    { label: '近 7 天总费用', value: formatUsd(data?.sevenDays.costUsd), detail: data ? costCoverage(data.sevenDays) : '', cost: true },
    { label: '近 30 天总请求', value: data ? formatInteger(data.thirtyDays.requestCount) : '—', detail: data ? `${data.thirtyDays.startDisplay} – ${data.thirtyDays.endDisplay}` : '', cost: false },
    { label: '近 30 天总费用', value: formatUsd(data?.thirtyDays.costUsd), detail: data ? costCoverage(data.thirtyDays) : '', cost: true },
    { label: '日均请求', value: data?.dailyAverageRequests != null ? averageFormatter.format(data.dailyAverageRequests) : '—', detail: averageDetail, cost: false },
    { label: '日均费用', value: formatUsd(data?.dailyAverageCostUsd), detail: averageDetail, cost: true },
  ]
})

const analyticsNotice = computed(() => {
  const analytics = props.statistics?.analytics
  if (!analytics || analytics.state === 'ready')
    return ''
  if (analytics.state === 'building')
    return '统计构建中，数据可能尚未完整'
  if (analytics.state === 'stale')
    return '统计暂未更新，当前显示上次可用结果'
  return `统计更新延迟 ${formatDuration(analytics.lagSeconds * 1000)}，最新请求可能尚未计入`
})
</script>

<template>
  <ZDialog v-model="open" title="账号统计" width="min(48rem, calc(100vw - 3rem))">
    <div ref="content" class="flex min-w-0 flex-col gap-4 outline-none" tabindex="-1" :aria-busy="loading">
      <section class="flex min-w-0 items-center gap-3 rounded-cp-card bg-cp-fill-quaternary px-4 py-3" aria-label="当前账号">
        <AccountIdentityCell :account="account" class="min-w-0 flex-1" size="lg" />
        <div class="hidden shrink-0 sm:block">
          <ProviderIconGroup :provider="account.provider" :authentication-kind="account.authenticationKind" />
        </div>
      </section>

      <div v-if="error" role="alert" class="min-w-0 rounded-cp-card bg-cp-error-container p-3 text-cp-sm text-cp-error-text">
        <p class="m-0 font-bold">
          {{ statistics ? '刷新失败，当前显示上次结果' : '统计加载失败' }}
        </p>
        <p class="mt-1 mb-0 whitespace-pre-wrap [overflow-wrap:anywhere]">
          {{ error }}
        </p>
        <ZButton class="mt-3" size="small" :disabled="loading" @click="refresh">
          重试
        </ZButton>
      </div>
      <p v-if="analyticsNotice" role="status" class="m-0 rounded-cp-card bg-cp-warning-container p-3 text-cp-sm text-cp-warning-text">
        {{ analyticsNotice }}
      </p>
      <p v-if="statistics?.thirtyDays.requestCount === 0" role="status" class="m-0 text-cp-sm text-cp-text-secondary">
        近 30 天暂无请求记录
      </p>

      <section v-if="loading || statistics" class="grid min-w-0 grid-cols-1 gap-3 sm:grid-cols-2" aria-label="账号使用统计">
        <article v-for="card in cards" :key="card.label" class="min-w-0 rounded-cp-card border border-cp-border-secondary bg-cp-fill-quaternary p-4">
          <h3 class="m-0 text-cp-sm font-bold text-cp-text-secondary">
            {{ card.label }}
          </h3>
          <div v-if="loading && !statistics" class="my-3 h-8 w-2/3 animate-pulse rounded bg-cp-fill-secondary motion-reduce:animate-none" aria-hidden="true" />
          <p v-else class="my-3 font-mono text-[26px] leading-tight font-heavy tabular-nums [overflow-wrap:anywhere]" :class="card.cost ? 'text-cp-success-text' : 'text-cp-text'">
            {{ card.value }}
          </p>
          <p class="m-0 min-h-4 text-cp-xs leading-relaxed text-cp-text-quaternary [overflow-wrap:anywhere]">
            {{ loading && !statistics ? '加载中' : card.detail }}
          </p>
        </article>
      </section>
      <span v-if="loading && !statistics" role="status" class="sr-only">正在加载账号统计</span>

      <div v-if="statistics" class="flex min-w-0 flex-wrap justify-between gap-x-4 gap-y-1 text-cp-xs text-cp-text-quaternary [overflow-wrap:anywhere]">
        <span>更新于 {{ statistics.generatedAtDisplay }}</span>
        <span>{{ statistics.timezone }} · 包含今天</span>
      </div>
      <p class="m-0 text-cp-xs leading-relaxed text-cp-text-quaternary">
        请求按逻辑请求统计，包含已记录的探测与预热
        <br>费用仅汇总成功交付请求的已知 USD 费用，未知费用不计作零
        <br>不代表上游平台实际扣款
      </p>
    </div>
    <template #footer>
      <ZButton @click="open = false">
        关闭
      </ZButton>
      <ZButton :loading="loading" :disabled="loading" @click="refresh">
        <RefreshCw class="mr-1.5 size-3.5" />
        刷新
      </ZButton>
    </template>
  </ZDialog>
</template>
