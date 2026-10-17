import type { Account, AccountStatistics } from '@/api'

import { shallowRef, watch } from 'vue'
import { getAccountStatistics } from '@/api'
import { useRequestState } from '@/composables/useRequestState'

export function useAccountStatistics() {
  const showStatistics = shallowRef(false)
  const statisticsAccount = shallowRef<Account | null>(null)
  const statistics = shallowRef<AccountStatistics | null>(null)
  const request = useRequestState()

  async function refresh() {
    const accountId = statisticsAccount.value?.id
    if (!showStatistics.value || !accountId || request.loading.value)
      return
    const requestId = request.start()
    try {
      const result = await getAccountStatistics(accountId, { signal: request.signal, silent: true })
      if (request.isCurrent(requestId))
        statistics.value = result
    }
    catch (error) {
      request.fail(requestId, error)
    }
    finally {
      request.finish(requestId)
    }
  }

  function open(account: Account) {
    request.invalidate()
    statisticsAccount.value = account
    statistics.value = null
    showStatistics.value = true
    void refresh()
  }

  watch(showStatistics, (visible) => {
    if (!visible)
      request.invalidate({ resetLoading: false })
  }, { flush: 'sync' })

  return {
    showStatistics,
    statisticsAccount,
    statistics,
    loading: request.loading,
    error: request.error,
    open,
    refresh,
  }
}
