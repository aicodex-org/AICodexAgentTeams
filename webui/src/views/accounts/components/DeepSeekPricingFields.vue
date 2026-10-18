<script setup lang="ts">
import type { DeepSeekPricingSchedule } from '@/api'
import { ZButton, ZCheckbox, ZFormItem, ZIconButton, ZInput, ZPopover, ZSwitch } from '@codex-proxy/ui'
import { CircleHelp, Plus, Trash2 } from '@lucide/vue'
import { computed, useId } from 'vue'
import { deepSeekPricingError, defaultDeepSeekPricingSchedule } from '../utils/deepseekPricing'

defineProps<{ disabled?: boolean }>()
const model = defineModel<DeepSeekPricingSchedule | undefined>({ required: true })
const headingId = useId()
const schedule = computed(() => model.value ?? defaultDeepSeekPricingSchedule())
const error = computed(() => deepSeekPricingError(schedule.value))
const weekdays = ['周一', '周二', '周三', '周四', '周五', '周六', '周日']

function update(changes: Partial<DeepSeekPricingSchedule>) {
  model.value = { ...schedule.value, ...changes }
}
function toggleWeekday(day: number, checked: boolean) {
  update({ peak_weekdays: checked ? [...schedule.value.peak_weekdays, day].sort((left, right) => left - right) : schedule.value.peak_weekdays.filter(value => value !== day) })
}
function updateWindow(index: number, field: 'start' | 'end', value: string) {
  update({ peak_windows: schedule.value.peak_windows.map((window, position) => position === index ? { ...window, [field]: value } : window) })
}
function addWindow() {
  update({ peak_windows: [...schedule.value.peak_windows, { start: '', end: '' }] })
}
</script>

<template>
  <section class="grid min-w-0 gap-4" :aria-labelledby="headingId">
    <div class="flex flex-wrap items-center justify-between gap-3">
      <div class="inline-flex min-w-0 items-center gap-1">
        <h3 :id="headingId" class="m-0 text-cp font-emphasis text-cp-text">
          费用估算
        </h3>
        <ZPopover trigger="hover-click" placement="top-start">
          <template #reference="{ open }">
            <ZIconButton aria-label="峰谷计价说明" :aria-expanded="open" class="size-6!">
              <CircleHelp class="size-3.5" aria-hidden="true" />
            </ZIconButton>
          </template>
          <div class="grid max-w-72 gap-2 p-3 text-cp-sm leading-relaxed text-cp-text-secondary">
            <p class="m-0">
              开启时按时区、日期和高峰时段选择价格，开始时间包含，结束时间不包含
            </p>
            <p class="m-0">
              跨午夜请拆成两段，24:00 仅可用于结束时间
            </p>
            <p class="m-0">
              仅影响新请求的本地估算与金额限额，历史费用不变，实际费用以 DeepSeek 账单为准
            </p>
          </div>
        </ZPopover>
      </div>
      <ZSwitch :model-value="schedule.enabled" label="峰谷计价" aria-label="峰谷计价" :disabled="disabled" @update:model-value="update({ enabled: $event })" />
    </div>
    <template v-if="schedule.enabled">
      <ZFormItem label="计价时区" required>
        <ZInput :model-value="schedule.timezone" aria-label="计价时区" placeholder="Asia/Shanghai" :disabled="disabled" @update:model-value="update({ timezone: $event })" />
      </ZFormItem>
      <ZFormItem label="高峰日期" required>
        <div class="flex flex-wrap gap-x-4 gap-y-3">
          <ZCheckbox v-for="(label, index) in weekdays" :key="label" :label="label" :model-value="schedule.peak_weekdays.includes(index + 1)" :disabled="disabled" @update:model-value="toggleWeekday(index + 1, $event)" />
        </div>
      </ZFormItem>
      <div class="flex items-start gap-1">
        <ZCheckbox :model-value="schedule.exclude_chinese_holidays" label="中国法定节假日按空闲价格" :disabled="disabled" @update:model-value="update({ exclude_chinese_holidays: $event })" />
        <ZPopover trigger="hover-click" placement="top-start">
          <template #reference="{ open }">
            <ZIconButton aria-label="中国节假日日历说明" :aria-expanded="open" class="size-6! shrink-0">
              <CircleHelp class="size-3.5" aria-hidden="true" />
            </ZIconButton>
          </template>
          <p class="m-0 max-w-72 p-3 text-cp-sm leading-relaxed text-cp-text-secondary">
            中国节假日按北京时间，内置日历覆盖 2026 年，未覆盖年份的高峰费用保留未知
          </p>
        </ZPopover>
      </div>
      <ZFormItem label="高峰时段" required>
        <div class="grid gap-3">
          <div v-for="(window, index) in schedule.peak_windows" :key="index" class="grid min-w-0 grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)_auto] items-center gap-2">
            <ZInput :model-value="window.start" :aria-label="`高峰时段 ${index + 1} 开始`" placeholder="09:00" maxlength="5" :disabled="disabled" @update:model-value="updateWindow(index, 'start', $event)" />
            <span class="text-cp-sm text-cp-text-tertiary">至</span>
            <ZInput :model-value="window.end" :aria-label="`高峰时段 ${index + 1} 结束`" placeholder="12:00" maxlength="5" :disabled="disabled" @update:model-value="updateWindow(index, 'end', $event)" />
            <ZIconButton :aria-label="`删除高峰时段 ${index + 1}`" :disabled="disabled || schedule.peak_windows.length <= 1" @click="update({ peak_windows: schedule.peak_windows.filter((_, position) => position !== index) })">
              <Trash2 class="size-4" aria-hidden="true" />
            </ZIconButton>
          </div>
          <ZButton class="justify-self-start" size="small" :disabled="disabled || schedule.peak_windows.length >= 8" @click="addWindow">
            <template #icon>
              <Plus class="size-3.5" aria-hidden="true" />
            </template>
            添加时段
          </ZButton>
        </div>
      </ZFormItem>
      <p class="m-0 text-cp-sm leading-relaxed text-cp-text-secondary">
        空闲时段为高峰以外的时间{{ schedule.exclude_chinese_holidays ? '，中国法定节假日按北京时间判断' : '' }}
      </p>
    </template>
    <p v-else class="m-0 text-cp-sm text-cp-text-secondary">
      全天按空闲价格估算，已保留时段设置
    </p>
    <p v-if="error" role="alert" class="m-0 text-cp-sm text-cp-error">
      {{ error }}
    </p>
  </section>
</template>
