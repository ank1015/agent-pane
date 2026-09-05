import { QueryClient } from '@tanstack/react-query'
import { ApiError } from './api-client'

const SECOND = 1_000
const MINUTE = 60 * SECOND

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30 * SECOND,
      gcTime: 10 * MINUTE,
      refetchOnWindowFocus: true,
      refetchOnReconnect: true,
      retry: (failureCount, error) => {
        if (error instanceof ApiError && error.status >= 400 && error.status < 500) {
          return false
        }

        return failureCount < 2
      },
    },
    mutations: {
      retry: false,
    },
  },
})
