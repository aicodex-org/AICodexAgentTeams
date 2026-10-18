import type { Account, ApiKeyConfiguration } from '@/api'
import { deepSeekPricingError, defaultDeepSeekPricingSchedule, parseDeepSeekPricingSchedule } from './deepseekPricing'

export function isApiKeyAccount(account: Pick<Account, 'provider' | 'authenticationKind'> | null | undefined): boolean {
  return Boolean(account && ['openai', 'deepseek'].includes(account.provider)) && account?.authenticationKind === 'api_key'
}

export function isOpenAiOAuthAccount(account: Pick<Account, 'provider' | 'authenticationKind'> | null | undefined): boolean {
  return account?.provider === 'openai' && account.authenticationKind === 'oauth'
}

export interface ApiKeyAccountForm extends ApiKeyConfiguration {
  name: string
  apiKey: string
}

export function emptyApiKeyAccountForm(provider?: string): ApiKeyAccountForm {
  return { pricing_schedule: provider === 'deepseek' ? defaultDeepSeekPricingSchedule() : undefined, name: '', base_url: provider === 'deepseek' ? 'https://api.deepseek.com' : '', apiKey: '', transport: 'http', protocols: provider === 'deepseek' ? ['openai', 'openai-chat', 'anthropic'] : undefined }
}

export function parseApiKeyConfiguration(value: Record<string, unknown> | undefined, provider?: string): ApiKeyConfiguration | undefined {
  if (!value || typeof value.base_url !== 'string' || (provider !== 'deepseek' && value.transport !== 'http' && value.transport !== 'prefer_websocket'))
    return undefined
  const schedule = provider === 'deepseek' ? parseDeepSeekPricingSchedule(value.pricing_schedule) : undefined
  if (provider === 'deepseek' && !schedule)
    return undefined
  return { pricing_schedule: schedule, base_url: value.base_url, transport: value.transport === 'prefer_websocket' ? 'prefer_websocket' : 'http', protocols: Array.isArray(value.protocols) ? value.protocols.filter((protocol): protocol is string => typeof protocol === 'string') : undefined }
}

export function apiKeyAccountError(form: ApiKeyAccountForm, editing = false): string | undefined {
  if (form.pricing_schedule) {
    const error = deepSeekPricingError(form.pricing_schedule)
    if (error)
      return error
  }
  if (!editing && !form.name.trim())
    return '请输入账号名称'
  if (form.base_url.trim().length > 2048)
    return '上游 API 地址不能超过 2048 个字符'
  try {
    const url = new URL(form.base_url)
    if (!['https:', 'http:'].includes(url.protocol) || !url.hostname || url.username || url.password || url.search || url.hash)
      return '请输入不含认证、查询参数或片段的 HTTP 或 HTTPS 地址'
  }
  catch {
    return '请输入完整的上游 API 地址'
  }
  if (form.protocols && form.protocols.length === 0)
    return '请至少选择一种协议'
  if (!editing && !form.apiKey)
    return '请输入 API Key'
  if (form.apiKey && (!/^[\x21-\x7E]+$/.test(form.apiKey) || form.apiKey.length > 16384))
    return 'API Key 不能包含空格或控制字符'
  return undefined
}
