import { useRef, useState } from 'react'
import { useMutation } from '@tanstack/react-query'
import { ApiError, postJson } from '../../lib/api-client'
import { retryRead } from './chat-queries'
import type { AcceptedRun, UserMessage } from './project-session-types'

export type SendAttempt = { key: string; path: string; body: unknown }
export function userMessage(prompt: string): UserMessage {
  return { role: 'user', id: crypto.randomUUID(), timestamp: Date.now(), content: [{ type: 'text', content: prompt.trim() }] }
}
// Keep the exact request across retries/reloads. Never reconstruct a timestamp,
// revision, config, or idempotency key after an ambiguous outcome.
export function useChatSubmission(scope: string, onAccepted: (reply: AcceptedRun) => void, onRejected?: () => void) {
  const storageKey = `chat-send:v1:${scope}`
  const [attempt, setAttempt] = useState<SendAttempt | null>(() => {
    try {
      const raw = sessionStorage.getItem(storageKey)
      if (!raw) return null
      const value = JSON.parse(raw)
      return typeof value.key === 'string' && typeof value.path === 'string' && value.path.startsWith('/api/') && value.body ? value : null
    } catch { return null }
  })
  const attemptRef = useRef(attempt)
  const mutation = useMutation({
    mutationFn: (request: SendAttempt) => postJson<AcceptedRun>(request.path, request.body, request.key),
    retry: retryRead, retryDelay: n => Math.min(1000 * 2 ** n, 5000),
    networkMode: 'always',
    onSuccess: reply => {
      attemptRef.current = null; setAttempt(null)
      try { sessionStorage.removeItem(storageKey) } catch { /* Storage can be unavailable. */ }
      onAccepted(reply)
    },
    onError: error => {
      onRejected?.()
      if (error instanceof ApiError && error.status >= 400 && error.status < 500 && ![408, 429].includes(error.status)) {
        attemptRef.current = null; setAttempt(null)
        try { sessionStorage.removeItem(storageKey) } catch { /* In-memory retry still works. */ }
      }
    },
  })
  const send = (path: string, body: unknown) => {
    if (mutation.isPending || attemptRef.current) return
    const next = { key: crypto.randomUUID(), path, body }
    attemptRef.current = next; setAttempt(next)
    try { sessionStorage.setItem(storageKey, JSON.stringify(next)) } catch { /* Keep exact request in memory. */ }
    mutation.mutate(next)
  }
  return { send, isPending: mutation.isPending, error: mutation.error, uncertain: attempt !== null && !mutation.isPending,
    retry: () => { if (attemptRef.current && !mutation.isPending) mutation.mutate(attemptRef.current) },
  }
}
