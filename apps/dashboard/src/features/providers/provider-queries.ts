import {
  useInfiniteQuery,
  useMutation,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query'
import {
  deleteRequest,
  getJson,
  patchJson,
  postJson,
  putJson,
} from '../../lib/api-client'

export type ProviderKind =
  | 'anthropic'
  | 'chatgpt'
  | 'deepseek'
  | 'fireworks'
  | 'openai'
  | 'openrouter'

export type ProviderAccount = {
  id: string
  name: string
  provider: ProviderKind
  status: 'enabled' | 'disabled'
  created_at: string
  is_default: boolean
}

export type ProviderUsageCost = {
  input?: number
  output?: number
  cache_read?: number
  cache_write?: number
  total: number
}

export type ProviderRequestUsage = {
  input?: number
  output?: number
  cache_read?: number
  cache_write?: number
  cost?: ProviderUsageCost
}

export type ProviderUsageSummary = {
  account_id: string
  request_count: number
  costs: {
    total: number
    input: number
    output: number
    cache_read: number
    cache_write: number
  }
  tokens: {
    input: number
    output: number
    cache_read: number
    cache_write: number
  }
}

export type ProviderRequest = {
  request_id: string
  requested_provider: string
  requested_model: string
  response_provider: string
  response_model: string
  assistant_message_id: string
  usage: ProviderRequestUsage | null
  duration_ms: number
  completed_at: string
}

export type ProviderRequestPage = {
  items: ProviderRequest[]
  next_cursor: string | null
}

export type ApiKeyProviderKind = Exclude<ProviderKind, 'chatgpt'>

export type CreateProviderAccountInput = {
  provider: ApiKeyProviderKind
  name: string
  apiKey: string
}

export type StartChatGptLoginResponse = {
  login_id: string
  authorization_url: string
}

export type ChatGptLoginStatus =
  | { status: 'pending' | 'exchanging' }
  | { status: 'succeeded'; provider_id: string }
  | { status: 'failed'; error: string }

type CreateProviderAccountResponse = {
  provider: {
    id: string
  }
}

export type SetProviderEnabledInput = {
  providerId: string
  enabled: boolean
}

export type UpdateProviderNameInput = {
  providerId: string
  name: string
}

type UpdateProviderAccountResponse = {
  provider: {
    id: string
    enabled: boolean
  }
}

type UpdateProviderNameResponse = {
  provider: {
    id: string
    name: string
  }
}

type SetDefaultProviderAccountResponse = {
  provider: {
    id: string
    is_default: boolean
  }
}

export const providerKeys = {
  all: ['providers'] as const,
  accounts: () => [...providerKeys.all, 'accounts'] as const,
  usage: (providerId: string) =>
    [...providerKeys.all, providerId, 'usage'] as const,
  requests: (providerId: string) =>
    [...providerKeys.all, providerId, 'requests'] as const,
}

export function useProviderAccounts() {
  return useQuery({
    queryKey: providerKeys.accounts(),
    queryFn: ({ signal }) => getJson<ProviderAccount[]>('/api/providers', signal),
  })
}

export function useProviderUsage(providerId: string) {
  return useQuery({
    queryKey: providerKeys.usage(providerId),
    queryFn: ({ signal }) =>
      getJson<ProviderUsageSummary>(
        `/api/providers/${encodeURIComponent(providerId)}/usage`,
        signal,
      ),
    enabled: providerId.length > 0,
    refetchInterval: 5_000,
  })
}

export function useProviderRequests(providerId: string) {
  return useInfiniteQuery({
    queryKey: providerKeys.requests(providerId),
    queryFn: ({ pageParam, signal }) => {
      const search = new URLSearchParams({ limit: '25' })
      if (pageParam !== null) {
        search.set('cursor', pageParam)
      }
      return getJson<ProviderRequestPage>(
        `/api/providers/${encodeURIComponent(providerId)}/requests?${search}`,
        signal,
      )
    },
    initialPageParam: null as string | null,
    getNextPageParam: (lastPage) => lastPage.next_cursor ?? undefined,
    enabled: providerId.length > 0,
    refetchInterval: 5_000,
  })
}

export function useCreateProviderAccount() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: ({ provider, name, apiKey }: CreateProviderAccountInput) =>
      postJson<CreateProviderAccountResponse>('/api/providers', {
        provider,
        name,
        api_key: apiKey,
      }),
    onSuccess: () =>
      queryClient.invalidateQueries({ queryKey: providerKeys.accounts() }),
  })
}

export function useStartChatGptLogin() {
  return useMutation({
    mutationFn: (name: string) =>
      postJson<StartChatGptLoginResponse>('/api/providers/chatgpt/login', {
        name,
      }),
  })
}

export function useChatGptLoginStatus(loginId: string | null) {
  return useQuery({
    queryKey: [...providerKeys.all, 'chatgpt-login', loginId] as const,
    queryFn: ({ signal }) =>
      getJson<ChatGptLoginStatus>(
        `/api/providers/chatgpt/login/${encodeURIComponent(loginId ?? '')}`,
        signal,
      ),
    enabled: loginId !== null,
    refetchInterval: (query) => {
      const status = query.state.data?.status
      return status === 'succeeded' || status === 'failed' ? false : 1_000
    },
    refetchIntervalInBackground: true,
    retry: 1,
  })
}

export function useCancelChatGptLogin() {
  return useMutation({
    mutationFn: (loginId: string) =>
      deleteRequest(
        `/api/providers/chatgpt/login/${encodeURIComponent(loginId)}`,
      ),
  })
}

export function useDeleteProviderAccount() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: (providerId: string) =>
      deleteRequest(`/api/providers/${encodeURIComponent(providerId)}`),
    onSuccess: (_, providerId) => {
      queryClient.setQueryData<ProviderAccount[]>(
        providerKeys.accounts(),
        (accounts) =>
          accounts?.filter((account) => account.id !== providerId),
      )
      return queryClient.invalidateQueries({ queryKey: providerKeys.accounts() })
    },
  })
}

export function useSetProviderAccountEnabled() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: ({ providerId, enabled }: SetProviderEnabledInput) =>
      patchJson<UpdateProviderAccountResponse>(
        `/api/providers/${encodeURIComponent(providerId)}`,
        { enabled },
      ),
    onMutate: async ({ providerId, enabled }) => {
      await queryClient.cancelQueries({ queryKey: providerKeys.accounts() })
      const previousAccounts = queryClient.getQueryData<ProviderAccount[]>(
        providerKeys.accounts(),
      )
      queryClient.setQueryData<ProviderAccount[]>(
        providerKeys.accounts(),
        (accounts) =>
          accounts?.map((account) =>
            account.id === providerId
              ? { ...account, status: enabled ? 'enabled' : 'disabled' }
              : account,
          ),
      )
      return { previousAccounts }
    },
    onError: (_error, _variables, context) => {
      queryClient.setQueryData(
        providerKeys.accounts(),
        context?.previousAccounts,
      )
    },
    onSettled: () =>
      queryClient.invalidateQueries({ queryKey: providerKeys.accounts() }),
  })
}

export function useUpdateProviderAccountName() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: ({ providerId, name }: UpdateProviderNameInput) =>
      patchJson<UpdateProviderNameResponse>(
        `/api/providers/${encodeURIComponent(providerId)}`,
        { name },
      ),
    onMutate: async ({ providerId, name }) => {
      await queryClient.cancelQueries({ queryKey: providerKeys.accounts() })
      const previousAccounts = queryClient.getQueryData<ProviderAccount[]>(
        providerKeys.accounts(),
      )
      queryClient.setQueryData<ProviderAccount[]>(
        providerKeys.accounts(),
        (accounts) =>
          accounts?.map((account) =>
            account.id === providerId ? { ...account, name } : account,
          ),
      )
      return { previousAccounts }
    },
    onError: (_error, _variables, context) => {
      queryClient.setQueryData(
        providerKeys.accounts(),
        context?.previousAccounts,
      )
    },
    onSettled: () =>
      queryClient.invalidateQueries({ queryKey: providerKeys.accounts() }),
  })
}

export function useSetDefaultProviderAccount() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: (providerId: string) =>
      putJson<SetDefaultProviderAccountResponse>(
        `/api/providers/${encodeURIComponent(providerId)}/default`,
      ),
    onMutate: async (providerId) => {
      await queryClient.cancelQueries({ queryKey: providerKeys.accounts() })
      const previousAccounts = queryClient.getQueryData<ProviderAccount[]>(
        providerKeys.accounts(),
      )
      const selectedAccount = previousAccounts?.find(
        (account) => account.id === providerId,
      )

      if (selectedAccount !== undefined) {
        queryClient.setQueryData<ProviderAccount[]>(
          providerKeys.accounts(),
          (accounts) =>
            accounts?.map((account) =>
              account.provider === selectedAccount.provider
                ? { ...account, is_default: account.id === providerId }
                : account,
            ),
        )
      }

      return { previousAccounts }
    },
    onError: (_error, _providerId, context) => {
      queryClient.setQueryData(
        providerKeys.accounts(),
        context?.previousAccounts,
      )
    },
    onSettled: () =>
      queryClient.invalidateQueries({ queryKey: providerKeys.accounts() }),
  })
}
