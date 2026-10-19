import type { ApiKeyConfiguration } from '@/api'

export type MiMoAccessMode = NonNullable<ApiKeyConfiguration['access_mode']>
export type MiMoCluster = 'api' | 'cn' | 'sgp' | 'ams' | 'custom'

const planHosts = {
  cn: 'token-plan-cn.xiaomimimo.com',
  sgp: 'token-plan-sgp.xiaomimimo.com',
  ams: 'token-plan-ams.xiaomimimo.com',
} as const

export function miMoDefaultBaseUrl(mode: MiMoAccessMode, cluster: 'cn' | 'sgp' | 'ams' = 'cn') {
  return `https://${mode === 'token_plan' ? planHosts[cluster] : 'api.xiaomimimo.com'}/v1`
}

export function inferMiMoAccessMode(value: Record<string, unknown>): MiMoAccessMode {
  if (value.access_mode === 'api_key' || value.access_mode === 'token_plan')
    return value.access_mode
  try {
    const origin = new URL(String(value.base_url ?? '')).origin
    return Object.values(planHosts).some(host => origin === `https://${host}`) ? 'token_plan' : 'api_key'
  }
  catch {
    return 'api_key'
  }
}

export function miMoCluster(baseUrl: string, mode: MiMoAccessMode): MiMoCluster {
  try {
    const url = new URL(baseUrl)
    if (url.username || url.password || url.search || url.hash || !['', '/v1'].includes(url.pathname.replace(/\/+$/, '')))
      return 'custom'
    if (mode === 'api_key')
      return url.origin === 'https://api.xiaomimimo.com' ? 'api' : 'custom'
    return (Object.keys(planHosts) as Array<keyof typeof planHosts>).find(cluster => url.origin === `https://${planHosts[cluster]}`) ?? 'custom'
  }
  catch {
    return 'custom'
  }
}

export function miMoBaseUrlAfterModeChange(baseUrl: string, previousMode: MiMoAccessMode, mode: MiMoAccessMode) {
  // 自定义中转路径由用户维护，切换密钥类型只替换官方根地址
  return !baseUrl.trim() || miMoCluster(baseUrl, previousMode) !== 'custom'
    ? miMoDefaultBaseUrl(mode)
    : baseUrl
}
