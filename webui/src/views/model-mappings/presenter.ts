import type { ModelMappingDraft, ModelMappingEntry } from '@/api/modules/model-mappings'

export const mappingProtocolOptions = [
  { label: 'Responses', value: 'openai' },
  { label: 'Chat Completions', value: 'openai-chat' },
  { label: 'Messages', value: 'anthropic' },
]

export function mappingProtocolLabel(protocol: string) {
  return mappingProtocolOptions.find(option => option.value === protocol)?.label ?? protocol
}

export type MappingField = keyof ModelMappingEntry
export interface MappingValidationIssue {
  field: MappingField
  message: string
}

const modelIdEncoder = new TextEncoder()

export function mappingValidationIssues(draft: ModelMappingDraft): (MappingValidationIssue | null)[] {
  const seen = new Set<string>()
  return draft.mappings.map((entry) => {
    if (!mappingProtocolOptions.some(option => option.value === entry.protocol))
      return { field: 'protocol', message: '请选择请求协议' }
    for (const [field, label] of [['requestedModel', '请求模型 ID'], ['routeModel', '路由模型 ID']] as const) {
      const value = entry[field]
      if (!value?.trim())
        return { field, message: `请输入${label}` }
      if (modelIdEncoder.encode(value).length > 256)
        return { field, message: `${label}不能超过 256 字节` }
      if (/\p{Cc}/u.test(value))
        return { field, message: `${label}不能包含控制字符` }
      if (value.startsWith('__'))
        return { field, message: `${label}不能使用 __ 保留前缀` }
      if (value.toLowerCase() === 'all' || /[\s*?,，;；]/u.test(value))
        return { field, message: `${label}必须是单个精确 ID，不支持 all 或通配符` }
    }
    if (!entry.targetProvider)
      return { field: 'targetProvider', message: '请选择目标平台' }
    const key = `${entry.protocol}\0${entry.requestedModel}`
    if (seen.has(key))
      return { field: 'requestedModel', message: '同一协议下的请求模型 ID 不能重复' }
    seen.add(key)
    return null
  })
}

export function mappingValidationErrors(draft: ModelMappingDraft) {
  return mappingValidationIssues(draft).map(issue => issue?.message ?? '')
}
