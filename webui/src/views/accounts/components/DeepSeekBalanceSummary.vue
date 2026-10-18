<script setup lang="ts">
import type { Account } from '@/api'
import { ZIconButton, ZPopover } from '@codex-proxy/ui'
import { ChevronDown, RefreshCw } from '@lucide/vue'

const props = defineProps<{ account: Account, refreshing: boolean }>()
const emit = defineEmits<{ refresh: [accountId: string] }>()

function balanceSummary(value: string) {
  const [integer, rawFraction = ''] = value.split('.')
  const fraction = rawFraction.replace(/0+$/, '')
  // 摘要省略长小数，弹层保留上游原值，不转为浮点数处理金额
  const precision = Math.max(2, (fraction.search(/[1-9]/) + 2))
  return `${integer}.${fraction.slice(0, precision).padEnd(2, '0')}${fraction.length > precision ? '…' : ''}`
}
</script>

<template>
  <div class="flex min-w-0 items-center gap-1 border-t border-cp-border-secondary pt-1.5" :aria-busy="refreshing">
    <span class="shrink-0 text-[10px] text-cp-text-quaternary">余额</span>
    <ZPopover v-if="account.quota.balances.length > 0" trigger="hover-click" placement="bottom-start">
      <template #reference="{ open }">
        <button
          type="button"
          class="flex min-w-0 flex-1 cursor-pointer items-center gap-1 rounded-cp border-0 bg-transparent p-0 text-left text-cp-xs text-cp-text outline-none hover:text-cp-primary-text focus-visible:ring-2 focus-visible:ring-cp-primary"
          aria-label="查看 DeepSeek 余额明细"
          :aria-expanded="open"
        >
          <span class="flex min-w-0 flex-wrap items-baseline gap-x-2 gap-y-0.5">
            <span v-for="(balance, index) in account.quota.balances" :key="`${balance.currency}-${index}`" class="min-w-0 truncate font-mono tabular-nums" :title="`${balance.currency} ${balance.totalBalance}`">
              <span class="text-[9px] text-cp-text-quaternary">{{ balance.currency }}</span>
              {{ balanceSummary(balance.totalBalance) }}
            </span>
          </span>
          <ChevronDown class="size-3 shrink-0 text-cp-text-quaternary" aria-hidden="true" />
        </button>
      </template>
      <div class="grid w-64 max-w-[calc(100vw-2rem)] gap-3 p-3 text-cp-xs text-cp-text-secondary">
        <div class="flex items-center justify-between gap-2">
          <strong class="text-cp-text">上游余额</strong>
          <span class="text-[10px] text-cp-text-quaternary">{{ account.quota.refreshedAtDisplay }}刷新</span>
        </div>
        <div v-for="(balance, index) in account.quota.balances" :key="`${balance.currency}-${index}`" class="grid gap-2 border-t border-cp-border-secondary pt-2">
          <div class="flex items-baseline justify-between gap-3">
            <span class="shrink-0 text-[10px] font-bold text-cp-text-tertiary">{{ balance.currency }}</span>
            <strong class="min-w-0 break-all text-right font-mono tabular-nums text-cp-text">{{ balance.totalBalance }}</strong>
          </div>
          <dl class="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1 text-[10px]">
            <dt class="text-cp-text-quaternary">
              赠送
            </dt>
            <dd class="m-0 break-all text-right font-mono tabular-nums">
              {{ balance.grantedBalance ?? '—' }}
            </dd>
            <dt class="text-cp-text-quaternary">
              充值
            </dt>
            <dd class="m-0 break-all text-right font-mono tabular-nums">
              {{ balance.toppedUpBalance ?? '—' }}
            </dd>
          </dl>
        </div>
      </div>
    </ZPopover>
    <span v-else class="min-w-0 flex-1 text-[10px] text-cp-text-quaternary">{{ refreshing ? '查询中…' : account.quota.refreshedAtDisplay === '—' ? '尚未查询' : '上游未返回余额' }}</span>
    <ZIconButton
      v-if="account.capabilities.quotaRefresh"
      size="small"
      class="size-5! ml-auto shrink-0"
      aria-label="刷新 DeepSeek 余额"
      :loading="refreshing"
      :disabled="refreshing"
      @click.stop="emit('refresh', props.account.id)"
    >
      <RefreshCw class="size-3" aria-hidden="true" />
    </ZIconButton>
  </div>
</template>
