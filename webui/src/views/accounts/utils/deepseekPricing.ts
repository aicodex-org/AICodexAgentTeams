import type { DeepSeekPricingSchedule } from '@/api'

export function defaultDeepSeekPricingSchedule(): DeepSeekPricingSchedule {
  return {
    enabled: true,
    timezone: 'Asia/Shanghai',
    peak_weekdays: [1, 2, 3, 4, 5],
    peak_windows: [{ start: '09:00', end: '12:00' }, { start: '14:00', end: '18:00' }],
    exclude_chinese_holidays: true,
  }
}

function timeMinutes(value: string, end = false): number | undefined {
  if (end && value === '24:00')
    return 1440
  if (!/^(?:[01]\d|2[0-3]):[0-5]\d$/.test(value))
    return undefined
  const [hours, minutes] = value.split(':').map(Number)
  return hours! * 60 + minutes!
}

export function deepSeekPricingError(schedule: DeepSeekPricingSchedule): string | undefined {
  try {
    new Intl.DateTimeFormat('en', { timeZone: schedule.timezone }).format()
    if (!schedule.timezone || /^[+-]/.test(schedule.timezone))
      return '请输入有效的 IANA 时区，例如 Asia/Shanghai'
  }
  catch {
    return '请输入有效的 IANA 时区，例如 Asia/Shanghai'
  }
  if (!schedule.peak_weekdays.length || schedule.peak_weekdays.some(day => !Number.isInteger(day) || day < 1 || day > 7) || new Set(schedule.peak_weekdays).size !== schedule.peak_weekdays.length)
    return '请至少选择一个高峰日期，日期不能重复'
  if (!schedule.peak_windows.length || schedule.peak_windows.length > 8)
    return '请配置 1～8 个高峰时段'
  const windows: { start: number, end: number }[] = []
  for (const window of schedule.peak_windows) {
    const start = timeMinutes(window.start)
    const end = timeMinutes(window.end, true)
    if (start === undefined || end === undefined || start >= end)
      return '时段须使用 HH:mm，结束晚于开始，跨午夜请拆分，结束可填 24:00'
    windows.push({ start, end })
  }
  windows.sort((left, right) => left.start - right.start)
  if (windows.some((window, index) => index > 0 && window.start < windows[index - 1]!.end))
    return '高峰时段不能重叠'
  return undefined
}

export function parseDeepSeekPricingSchedule(value: unknown): DeepSeekPricingSchedule | undefined {
  if (value === undefined)
    return defaultDeepSeekPricingSchedule()
  if (!value || typeof value !== 'object' || Array.isArray(value))
    return undefined
  const schedule = value as Record<string, unknown>
  if (typeof schedule.enabled !== 'boolean' || typeof schedule.timezone !== 'string' || typeof schedule.exclude_chinese_holidays !== 'boolean'
    || !Array.isArray(schedule.peak_weekdays) || !schedule.peak_weekdays.every(day => typeof day === 'number')
    || !Array.isArray(schedule.peak_windows) || !schedule.peak_windows.every(window => window && typeof window === 'object' && typeof window.start === 'string' && typeof window.end === 'string')) {
    return undefined
  }
  const result: DeepSeekPricingSchedule = {
    enabled: schedule.enabled,
    timezone: schedule.timezone,
    peak_weekdays: [...schedule.peak_weekdays],
    peak_windows: schedule.peak_windows.map(window => ({ start: window.start, end: window.end })),
    exclude_chinese_holidays: schedule.exclude_chinese_holidays,
  }
  return deepSeekPricingError(result) ? undefined : result
}
