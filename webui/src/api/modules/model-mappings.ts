import type { RequestOptions } from '../request'
import type { AccountGroupPageMeta } from './groups'
import request from '../request'

export type MappingProtocol = 'openai' | 'openai-chat' | 'anthropic'

export interface ModelMappingEntry {
  protocol: MappingProtocol
  requestedModel: string
  routeModel: string
  targetProvider: string
}

export interface ModelMappingPolicy {
  id: string
  groupId: string
  groupName: string
  enabled: boolean
  remark: string | null
  revision: number
  createdAt: string
  createdAtDisplay: string
  updatedAt: string
  updatedAtDisplay: string
  mappings: ModelMappingEntry[]
}

export interface ModelMappingDraft {
  groupId: string
  enabled: boolean
  remark: string
  mappings: ModelMappingEntry[]
}

interface PolicyPage {
  items: ModelMappingPolicy[]
  page: AccountGroupPageMeta
  configRevision: number
}

interface PolicyMutation {
  id: string
  record: ModelMappingPolicy | null
  configRevision: number
}

export function getModelMappingPolicies(params: { page: number, pageSize: number, search?: string, enabled?: boolean }, options: RequestOptions = {}) {
  return request<PolicyPage>({
    url: '/api/admin/model-mapping-policies',
    method: 'GET',
    params,
    ...options,
  })
}

export function createModelMappingPolicy(data: ModelMappingDraft) {
  return request<PolicyMutation>({
    url: '/api/admin/model-mapping-policies/create',
    method: 'POST',
    data,
  })
}

export function updateModelMappingPolicy(data: Omit<ModelMappingDraft, 'groupId'> & { id: string, expectedRevision: number }) {
  return request<PolicyMutation>({
    url: '/api/admin/model-mapping-policies/update',
    method: 'POST',
    data,
  })
}

export function changeModelMappingPolicy(action: 'enable' | 'disable' | 'delete', data: { id: string, expectedRevision: number }) {
  return request<PolicyMutation>({
    url: `/api/admin/model-mapping-policies/${action}`,
    method: 'POST',
    data,
  })
}

export interface ModelMappingTarget {
  protocol: MappingProtocol
  provider: string
}

export function getModelMappingTargets(groupId: string, options: RequestOptions = {}) {
  return request<{ targets: ModelMappingTarget[] }>({
    url: '/api/admin/model-mapping-policies/options',
    method: 'GET',
    params: { groupId },
    ...options,
  })
}

export function previewModelMapping(data: {
  groupId: string
  policyId?: string
  revision?: number
  enabled: boolean
  mappings: ModelMappingEntry[]
  clientKeyId: string
  protocol: MappingProtocol
  requestedModel: string
}, options: RequestOptions = {}) {
  return request<{ configRevision: number, modelMapping: import('./usage').UsageModelMapping | null, targets: { provider: string, routeModel: string }[] }>({
    url: '/api/admin/model-mapping-policies/preview',
    method: 'POST',
    data,
    ...options,
  })
}
