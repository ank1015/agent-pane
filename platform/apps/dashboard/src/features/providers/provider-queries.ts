import { queryOptions, useInfiniteQuery, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { ProviderUsage, ProviderRequestPage } from './provider-types'
import { deleteRequest, getJson, postJson } from '../../lib/api-client'
import type { ProviderAccount, ProviderDetailResponse, CreateProviderInput, ChatGptLoginStart, ChatGptLoginStatus } from './provider-types'

export const providerKeys = {
  all: ['providers'] as const,
  accounts: () => [...providerKeys.all, 'accounts'] as const,
  detail: (providerId: string) => [...providerKeys.all, 'detail', providerId] as const,
  usage: (id: string) => [...providerKeys.all, 'usage', id] as const,
  requests: (id: string) => [...providerKeys.all, 'requests', id] as const,
}

export const providerAccountsOptions = queryOptions({
  queryKey: providerKeys.accounts(),
  queryFn: ({ signal }) => getJson<ProviderAccount[]>('/api/providers', signal),
  staleTime: 30_000,
  gcTime: 10 * 60_000,
  refetchOnWindowFocus: true,
  refetchOnReconnect: true,
  refetchInterval: 15_000,
  refetchIntervalInBackground: false,
  // Inherit the shared bounded retry policy: no retries for client errors.
})

export function useProviderAccounts() {
  return useQuery(providerAccountsOptions)
}

export function providerDetailOptions(providerId: string) {
  return queryOptions({
    queryKey: providerKeys.detail(providerId),
    queryFn: ({ signal }) => getJson<ProviderDetailResponse>(`/api/providers/${encodeURIComponent(providerId)}`, signal),
    enabled: providerId.length > 0,
    staleTime: 30_000,
    gcTime: 10 * 60_000,
    refetchOnWindowFocus: true,
    refetchOnReconnect: true,
    refetchInterval: 15_000,
    refetchIntervalInBackground: false,
  })
}

export function useProviderDetail(providerId: string) {
  return useQuery(providerDetailOptions(providerId))
}

// Independent queries avoid an account-detail -> analytics network waterfall.
const analyticsPolicy = {
  staleTime: 5_000,
  gcTime: 10 * 60_000,
  refetchInterval: 5_000,
  refetchIntervalInBackground: false,
  refetchOnWindowFocus: true,
  refetchOnReconnect: true,
}

export function useProviderUsage(id: string) {
  return useQuery({
    ...analyticsPolicy,
    queryKey: providerKeys.usage(id),
    queryFn: ({ signal }) => getJson<ProviderUsage>(`/api/providers/${encodeURIComponent(id)}/usage`, signal),
    enabled: id.length > 0,
  })
}

export function useProviderRequests(id: string) {
  return useInfiniteQuery({
    ...analyticsPolicy,
    queryKey: providerKeys.requests(id),
    queryFn: ({ pageParam, signal }) => {
      const search = new URLSearchParams({ limit: '25' })
      if (pageParam) search.set('cursor', pageParam)
      return getJson<ProviderRequestPage>(`/api/providers/${encodeURIComponent(id)}/requests?${search}`, signal)
    },
    enabled: id.length > 0,
    initialPageParam: null as string | null,
    getNextPageParam: (last, _pages, previousCursor) =>
      last.next_cursor && last.next_cursor !== previousCursor ? last.next_cursor : undefined,
  })
}

export function useCreateProviderAccount() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (input: CreateProviderInput) => postJson<ProviderAccount>('/api/providers', input),
    retry: false,
    gcTime: 0,
    onSuccess: async (account) => {
      await client.cancelQueries({ queryKey: providerKeys.accounts() })
      client.setQueryData<ProviderAccount[]>(providerKeys.accounts(), (previous = []) =>
        [...previous.filter((existing) => existing.id !== account.id), account].sort((a, b) =>
          a.provider.localeCompare(b.provider) || b.created_at.localeCompare(a.created_at) || a.id.localeCompare(b.id)))
      void client.invalidateQueries({ queryKey: providerKeys.accounts() })
    },
    onSettled: (_data, _error, input) => { input.api_key = '' },
  })
}

const loginPath = '/api/providers/chatgpt/login'
export const cancelChatGptLogin = (id: string) => deleteRequest(`${loginPath}/${encodeURIComponent(id)}`)

export function useStartChatGptLogin() {
  return useMutation({ mutationFn: (name: string) => postJson<ChatGptLoginStart>(loginPath, { name }), retry: false, gcTime: 0 })
}

export function useChatGptLoginStatus(id: string | null) {
  return useQuery({
    queryKey: [...providerKeys.all, 'chatgpt-login', id],
    queryFn: ({ signal }) => getJson<ChatGptLoginStatus>(`${loginPath}/${encodeURIComponent(id!)}`, signal),
    enabled: id !== null,
    staleTime: 0,
    gcTime: 0,
    refetchInterval: (query) => query.state.error || ['succeeded', 'failed'].includes(query.state.data?.status ?? '') ? false : 1000,
    // Continue while the popup has focus. The query is mounted only for an active login.
    refetchIntervalInBackground: true,
  })
}
