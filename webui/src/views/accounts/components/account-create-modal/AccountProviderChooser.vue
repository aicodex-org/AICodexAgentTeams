<script setup lang="ts">
import type { AccountCreateSource } from '../../utils/accountCreate'
import { Check, LayoutGrid } from '@lucide/vue'
import { computed, useId } from 'vue'
import { formatProviderLabel, PROVIDER_IDS, providerIcon } from '@/utils/providers'

defineProps<{ disabled: boolean }>()
const source = defineModel<AccountCreateSource | null>({ required: true })
const groupName = useId()
const options = [
  { value: 'bundle', label: '批量导入', icon: LayoutGrid },
  ...PROVIDER_IDS.map(provider => ({
    value: `provider:${provider}`,
    label: formatProviderLabel(provider),
    icon: providerIcon(provider),
  })),
]
const selected = computed({
  get: () => source.value?.kind === 'provider' ? `provider:${source.value.id}` : source.value?.kind ?? '',
  set: (value: string) => {
    if (!options.some(option => option.value === value))
      return
    source.value = value === 'bundle' ? { kind: 'bundle' } : { kind: 'provider', id: value.slice('provider:'.length) }
  },
})
</script>

<template>
  <!-- 按容器宽度换行，平台增加时仍完整展示；原生单选框保留方向键选择与禁用行为 -->
  <div
    class="grid min-w-0 grid-cols-[repeat(auto-fit,minmax(min(100%,10rem),1fr))] gap-2"
    role="radiogroup"
    aria-label="选择账号平台"
  >
    <label
      v-for="option in options"
      :key="option.value"
      :for="`${groupName}-${option.value}`"
      class="relative min-w-0"
    >
      <input
        :id="`${groupName}-${option.value}`"
        v-model="selected"
        type="radio"
        :name="groupName"
        :value="option.value"
        :disabled="disabled"
        class="peer sr-only"
      >
      <span
        class="flex min-h-14 items-center gap-3 rounded-cp px-3 py-2.5 text-cp font-emphasis transition-[background-color,color,box-shadow] duration-150 peer-focus-visible:ring-2 peer-focus-visible:ring-cp-control-outline motion-reduce:transition-none"
        :class="[
          selected === option.value
            ? 'bg-cp-primary-container text-cp-primary-on-container shadow-cp-tertiary'
            : 'bg-cp-fill-quaternary text-cp-text-secondary',
          disabled
            ? 'cursor-not-allowed opacity-60'
            : selected === option.value
              ? 'cursor-pointer'
              : 'cursor-pointer hover:bg-cp-bg-text-hover hover:text-cp-text',
        ]"
      >
        <component :is="option.icon" class="size-5 shrink-0" aria-hidden="true" />
        <span class="min-w-0 flex-1">{{ option.label }}</span>
        <Check
          class="size-4 shrink-0"
          :class="selected === option.value ? 'opacity-100' : 'opacity-0'"
          aria-hidden="true"
        />
      </span>
    </label>
  </div>
</template>
