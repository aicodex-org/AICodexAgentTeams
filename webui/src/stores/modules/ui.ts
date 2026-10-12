import { defineStore } from 'pinia'
import { shallowRef } from 'vue'

export const useUiStore = defineStore(
  'ui',
  () => {
    const sidebarCollapsed = shallowRef(false)

    function toggleSidebar(): void {
      sidebarCollapsed.value = !sidebarCollapsed.value
    }

    return {
      sidebarCollapsed,
      toggleSidebar,
    }
  },
  {
    persist: {
      key: 'AICodexAgentTeams-ui',
      pick: ['sidebarCollapsed'],
    },
  },
)
