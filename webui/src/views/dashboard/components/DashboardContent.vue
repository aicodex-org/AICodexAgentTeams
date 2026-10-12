<script setup lang="ts">
import type { dashboardSnapshotView, dashboardTrendView } from '../presenter'
import type { DashboardTrendKind } from '@/api/modules/dashboard'
import { ZButton, ZPageHeader } from '@codex-proxy/ui'

import { ArrowUpRight, RefreshCw } from '@lucide/vue'
import { defineAsyncComponent } from 'vue'
import { RouterLink } from 'vue-router'

import RequestHealthTimelineCard from '@/components/usage/RequestHealthTimelineCard.vue'
import AccountOverviewCard from './AccountOverviewCard.vue'
import DashboardHeartbeat from './DashboardHeartbeat.vue'
import MetricCard from './MetricCard.vue'
import RequestTrendCard from './RequestTrendCard.vue'
import UsageRecordCard from './UsageRecordCard.vue'
import WireProfileCard from './WireProfileCard.vue'

withDefaults(
  defineProps<{
    loading?: boolean
    refreshing?: boolean
    lastRefreshedAt?: string
    metrics: DashboardSnapshotView['metrics']
    analytics: DashboardSnapshotView['analytics']
    analyticsError?: string
    trendPoints: DashboardTrendView['points']
    trendSummary: DashboardTrendView['summary']
    trendLoading?: boolean
    trendError?: string
    healthTimeline: DashboardSnapshotView['healthTimeline']
    accountUsage: DashboardSnapshotView['accountUsage']
    wireProfiles: DashboardSnapshotView['wireProfiles']
    usageRecords: DashboardSnapshotView['usageRecords']
    poolSummary: DashboardSnapshotView['poolSummary']
    capacityInfo: DashboardSnapshotView['capacityInfo']
    rotationStrategy: DashboardSnapshotView['rotationStrategy']
  }>(),
  {
    loading: false,
    refreshing: false,
    lastRefreshedAt: '',
    analyticsError: '',
    trendLoading: false,
    trendError: '',
  },
)

const emit = defineEmits<{
  refresh: []
  trendChange: [kind: DashboardTrendKind]
}>()

const LaniakeaScene = defineAsyncComponent(() => import('@/components/cosmic/LaniakeaScene.vue'))

type DashboardSnapshotView = ReturnType<typeof dashboardSnapshotView>
type DashboardTrendView = ReturnType<typeof dashboardTrendView>

const trendKind = defineModel<DashboardTrendKind>('trendKind', { required: true })
</script>

<template>
  <div class="dashboard-content w-full">
    <div class="dashboard-universe" aria-hidden="true" inert>
      <LaniakeaScene :playing="false" decorative />
    </div>
    <ZPageHeader title="系统概览" class="dashboard-header">
      <template #description>
        <span class="period-label">当日统计</span>
        <DashboardHeartbeat :updated-at="lastRefreshedAt" />
      </template>
      <template #actions>
        <RouterLink to="/" class="home-link">
          返回主页 <ArrowUpRight :size="15" />
        </RouterLink>
        <ZButton
          class="refresh-button"
          aria-label="刷新概览"
          :loading="loading || refreshing"
          :disabled="loading || refreshing"
          @click="emit('refresh')"
        >
          <template #loading>
            <RefreshCw class="animate-spin motion-reduce:animate-none" :size="19" />
          </template>
          <RefreshCw :size="15" />
          刷新
        </ZButton>
      </template>
    </ZPageHeader>

    <p v-if="analyticsError" class="mt-4 text-cp-sm text-cp-warning-text" role="alert">
      {{ analytics ? '刷新失败，显示上次统计' : '统计暂不可用，请稍后重试' }}
    </p>

    <template v-if="!analyticsError || analytics">
      <section
        class="mt-5 grid grid-cols-1 gap-4 sm:grid-cols-2 xl:grid-cols-4"
        aria-label="核心指标"
      >
        <MetricCard v-for="metric in metrics" :key="metric.title" :metric="metric" />
      </section>

      <section
        class="mt-5 grid grid-cols-1 gap-5 xl:grid-cols-[minmax(0,1.5fr)_minmax(340px,1fr)]"
      >
        <RequestTrendCard
          v-model:kind="trendKind"
          :points="trendPoints"
          :summary="trendSummary"
          :loading="trendLoading"
          :error="trendError"
          @trend-change="emit('trendChange', $event)"
        />
        <WireProfileCard :profiles="wireProfiles" />
      </section>

      <AccountOverviewCard
        :accounts="accountUsage"
        :pool="poolSummary"
        :capacity="capacityInfo"
        :rotation-strategy="rotationStrategy"
        class="mt-5"
      />

      <RequestHealthTimelineCard :timeline="healthTimeline" class="mt-5" />

      <UsageRecordCard :rows="usageRecords" class="mt-5" />
    </template>
  </div>
</template>

<style scoped>
.dashboard-content {
  --cp-card-bg: color-mix(in srgb, var(--cp-color-bg-container) 94%, transparent);
  position: relative;
  isolation: isolate;
}
.dashboard-universe {
  position: absolute;
  inset: -24px 0 auto;
  height: 500px;
  z-index: -1;
  overflow: hidden;
  pointer-events: none;
  opacity: 0.25;
  mask-image: linear-gradient(#000, transparent);
}
.dashboard-header {
  padding: 12px 0 24px;
  border-bottom: 1px solid color-mix(in srgb, var(--cosmic-accent) 18%, transparent);
}
.dashboard-header :deep(h1) {
  font-size: 30px;
  font-weight: 500;
  letter-spacing: 1px;
}
.dashboard-header :deep(p) {
  flex-wrap: wrap;
  gap: 14px;
  font-size: 12px;
  font-weight: 400;
}
.period-label {
  color: var(--cosmic-accent);
}
.home-link {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  margin-right: 12px;
  color: var(--cp-color-text-secondary);
  font-size: 12px;
}
.home-link:hover {
  color: var(--cosmic-accent);
}
.refresh-button {
  color: var(--cosmic-accent);
  background: color-mix(in srgb, var(--cosmic-accent) 10%, var(--cp-color-bg-container));
}
.dashboard-content :deep(h2) {
  font-size: 17px;
  font-weight: 550;
  line-height: 1.4;
}
@media (max-width: 600px) {
  .dashboard-header {
    padding-top: 6px;
  }
  .dashboard-header :deep(h1) {
    font-size: 26px;
  }
}
</style>
