<script setup lang="ts">
import type { Account, AccountResetCredit } from '@/api'
import { ZButton, ZDialog, ZEmpty, ZIconButton, ZSegmented } from '@codex-proxy/ui'

import { AlertTriangle, RefreshCw, TicketCheck } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'
import { useAccountResetCredits } from '../../composables/useAccountResetCredits'
import AccountQuotaCredits from './Credits.vue'
import UsageLimits from './UsageLimits.vue'

const props = defineProps<{
  account: Account
}>()

const emit = defineEmits<{
  consumed: [accountId: string]
}>()

const panelOpen = defineModel<boolean>({ default: false })
const view = shallowRef('available')
const confirmation = shallowRef<{ credit?: AccountResetCredit, ambiguous: boolean } | null>(null)
const {
  credits,
  availableCredits,
  availableCount,
  consumptionCredit,
  canRequestConsume,
  hasSnapshot,
  loading,
  consuming,
  loadError,
  ambiguous,
  showConfirm,
  loadCredits,
  selectCredit,
  requestConsume,
  cancelConsume,
  confirmConsume,
} = useAccountResetCredits({
  accountId: () => props.account.id,
  capabilities: () => props.account.capabilities,
  onConsumed: (accountId) => {
    if (props.account.id === accountId)
      panelOpen.value = false
    emit('consumed', accountId)
  },
})

const modalTitle = computed(() => {
  if (!confirmation.value)
    return '使用限额重置'
  return confirmation.value.ambiguous ? '确认上次重置' : '确认重置额度'
})
const confirmCreditTitle = computed(() => confirmation.value?.credit
  ? creditTitle(confirmation.value.credit)
  : '使用一次重置（由上游选择）')
const creditItems = computed(() => availableCredits.value.map(credit => ({
  id: credit.id,
  title: creditTitle(credit),
  expiry: credit.expiresAtDisplay ? `将于 ${credit.expiresAtDisplay} 到期` : '有效期由上游决定',
})))
const historyCredits = computed(() => credits.value.filter(credit => credit.status !== 'available'))
const viewOptions = computed(() => [
  { label: hasSnapshot.value ? `可用 ${availableCount.value}` : '可用', value: 'available' },
  { label: '历史记录', value: 'history' },
])
const showCountOnlyAction = computed(() => view.value === 'available' && !loadError.value
  && hasSnapshot.value
  && availableCount.value > 0
  && creditItems.value.length === 0)
watch(panelOpen, (isOpen) => {
  if (isOpen) {
    confirmation.value = null
    view.value = 'available'
    void loadCredits()
    return
  }
  if (showConfirm.value)
    cancelConsume()
}, { immediate: true })

watch(showConfirm, (confirming) => {
  if (panelOpen.value)
    confirmation.value = confirming ? { credit: consumptionCredit.value, ambiguous: ambiguous.value } : null
})

function creditTitle(credit: AccountResetCredit | undefined) {
  return credit?.title?.trim() || '用量重置'
}

function historyStatus(status: string | null) {
  switch (status) {
    case 'redeemed': return '已使用'
    case 'redeeming': return '处理中'
    case 'expired': return '已过期'
    default: return status ? `状态：${status}` : '状态未知'
  }
}

function handleRequestConsume(creditId: string) {
  if (!props.account.capabilities.consumeResetCredit || loading.value || consuming.value || ambiguous.value)
    return
  selectCredit(creditId)
  requestConsume()
}
</script>

<template>
  <ZDialog
    v-model="panelOpen"
    :title="modalTitle"
    :type="confirmation ? 'warning' : 'default'"
    :width="confirmation ? 'min(28rem, calc(100vw - 2rem))' : 'min(36rem, calc(100vw - 2rem))'"
    :show-close="!consuming" :close-on-click-modal="!consuming" :close-on-press-escape="!consuming"
  >
    <div v-if="confirmation" class="grid gap-3">
      <section class="rounded-cp bg-cp-fill-quaternary px-4 py-3.5">
        <p class="m-0 text-cp-xs font-heavy text-cp-text-quaternary">
          本次使用
        </p>
        <p class="mt-1.5 mb-0 text-cp-lg leading-snug font-heavy text-cp-text">
          {{ confirmCreditTitle }}
        </p>
        <p
          v-if="confirmation.credit"
          class="mt-1 mb-0 font-mono text-[10px] leading-normal font-emphasis text-cp-text-quaternary"
        >
          {{ confirmation.credit.expiresAtDisplay ? `将于 ${confirmation.credit.expiresAtDisplay} 到期` : '有效期由上游决定' }}
        </p>
      </section>
    </div>

    <div v-else class="grid gap-4">
      <div class="grid min-w-0 gap-1.5">
        <p class="m-0 text-cp-sm font-heavy wrap-anywhere text-cp-text">
          {{ account.name || account.email || account.id }}
        </p>
        <p class="m-0 text-cp-xs leading-normal text-cp-text-secondary">
          使用一次重置，恢复对应的 5 小时限额、每周限额或两者
        </p>
      </div>

      <section v-if="ambiguous" class="flex items-start gap-3 rounded-cp bg-cp-warning-container px-4 py-3.5" role="status">
        <AlertTriangle class="mt-0.5 size-4 shrink-0 text-cp-warning-on-container" />
        <div class="min-w-0 flex-1">
          <p class="m-0 text-cp-sm font-heavy text-cp-warning-on-container">
            上次操作结果待确认
          </p>
          <p class="mt-1 mb-0 text-cp-xs leading-normal font-emphasis text-cp-text-secondary">
            请继续确认上次重置结果
          </p>
        </div>
        <ZButton size="small" type="primary" variant="plain" :disabled="loading || consuming || !canRequestConsume" @click="requestConsume">
          继续确认
        </ZButton>
      </section>

      <section class="overflow-hidden rounded-cp bg-cp-fill-quaternary" aria-label="使用限额重置">
        <div
          class="flex items-center gap-3 pt-4 pr-3 pl-4"
          :class="showCountOnlyAction ? 'pb-4' : 'pb-1'"
        >
          <ZSegmented v-model="view" class="min-w-0 flex-1" size="small" aria-label="重置额度列表" :options="viewOptions" :disabled="consuming" />
          <ZIconButton
            size="small"
            aria-label="刷新重置额度"
            :loading="loading"
            :disabled="loading || consuming"
            @click="loadCredits"
          >
            <template #loading>
              <RefreshCw class="size-3.5 animate-spin motion-reduce:animate-none" />
            </template>
            <RefreshCw class="size-3.5" />
          </ZIconButton>
        </div>

        <div :aria-busy="loading || consuming">
          <p
            v-if="loadError"
            class="mx-4 mt-0 mb-4 rounded-cp bg-cp-error-container px-4 py-3 text-cp-xs leading-normal font-emphasis text-cp-error-on-container"
            role="status"
          >
            {{ loadError }}，请刷新重试
          </p>
          <p
            v-else-if="loading && !hasSnapshot"
            class="m-0 px-4 pt-1 pb-4 text-cp-xs font-emphasis text-cp-text-secondary"
            role="status"
          >
            正在查询可用重置次数…
          </p>
          <ul v-else-if="view === 'history' && historyCredits.length" class="m-0 list-none px-0 pt-0 pb-1">
            <li v-for="credit in historyCredits" :key="credit.id" class="flex flex-wrap items-center gap-3 px-4 py-3">
              <div class="min-w-0 flex-1">
                <p class="m-0 text-cp-sm font-heavy wrap-anywhere text-cp-text">
                  {{ creditTitle(credit) }}
                </p>
                <p class="mt-1 mb-0 text-cp-xs leading-normal wrap-anywhere text-cp-text-secondary">
                  {{ credit.expiresAtDisplay ? `到期时间 ${credit.expiresAtDisplay}` : '上游未提供到期时间' }}
                </p>
              </div>
              <span class="text-cp-xs wrap-anywhere text-cp-text-secondary">{{ historyStatus(credit.status) }}</span>
            </li>
          </ul>
          <ZEmpty
            v-else-if="view === 'history'"
            :icon="TicketCheck"
            size="small"
            surface="none"
            title="暂无重置历史记录"
            description="仅展示上游返回的记录"
          />
          <ul v-else-if="creditItems.length" class="m-0 list-none px-0 pt-0 pb-1">
            <li
              v-for="credit in creditItems"
              :key="credit.id"
              class="flex flex-wrap items-center gap-4 px-4 py-3"
            >
              <div class="min-w-0 flex-1">
                <p class="m-0 text-cp-sm leading-normal font-heavy wrap-anywhere text-cp-text">
                  {{ credit.title }}
                </p>
                <p class="mt-1 mb-0 text-cp-xs leading-normal font-emphasis text-cp-text-secondary">
                  {{ credit.expiry }}
                </p>
              </div>
              <ZButton
                size="small"
                type="primary"
                :disabled="!account.capabilities.consumeResetCredit || loading || consuming || ambiguous || availableCount <= 0"
                :aria-label="`使用重置：${credit.title}，${credit.expiry}`"
                @click="handleRequestConsume(credit.id)"
              >
                使用重置额度
              </ZButton>
            </li>
          </ul>

          <ZEmpty
            v-else-if="!showCountOnlyAction"
            :icon="TicketCheck"
            size="small"
            surface="none"
            title="当前没有可用重置次数"
            description="可刷新列表，重新读取上游状态"
          />
        </div>
      </section>

      <UsageLimits :windows="account.quota.windows" />
      <AccountQuotaCredits v-if="account.quota.credits" :credits="account.quota.credits" />
    </div>

    <template v-if="confirmation || showCountOnlyAction" #footer>
      <template v-if="confirmation">
        <ZButton :disabled="consuming" @click="cancelConsume">
          返回
        </ZButton>
        <ZButton type="primary" :loading="consuming" :disabled="loading || !canRequestConsume" @click="confirmConsume">
          {{ confirmation.ambiguous ? '再次确认' : '确认重置' }}
        </ZButton>
      </template>
      <ZButton
        v-else
        size="small"
        type="primary"
        :disabled="loading || consuming || ambiguous || !canRequestConsume"
        @click="requestConsume"
      >
        使用一次重置（由上游选择）
      </ZButton>
    </template>
  </ZDialog>
</template>
