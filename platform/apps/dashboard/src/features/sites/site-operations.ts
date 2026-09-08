import { useEffect, useRef, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ApiError, getJson, postJson } from '../../lib/api-client'
import { siteKeys, sitePath } from './site-queries'
export type SiteOperation = { id: string; siteId: string; status: 'pending' | 'succeeded' | 'conflict'; releaseId?: string; error?: string }
type Attempt = { id: string; kind: 'snapshot'; name: string } | { id: string; kind: 'restore'; snapshot: string }
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i
export function readSiteAttempt(raw: string | null): Attempt | null {
  try {
    const a = JSON.parse(raw || 'null')
    if (!a || !uuid.test(a.id || '')) return null
    if (a.kind === 'snapshot' && typeof a.name === 'string' && a.name.trim() && new TextEncoder().encode(a.name).length <= 128) return { id: a.id, kind: a.kind, name: a.name }
    if (a.kind === 'restore' && uuid.test(a.snapshot || '')) return { id: a.id, kind: a.kind, snapshot: a.snapshot }
  } catch { /* Ignore invalid saved state. */ }
  return null
}
export function useSiteOperation(project: string, site: string) {
  const client = useQueryClient(), storageKey = `site-operation:v1:${project}:${site}`
  const [attempt, setAttempt] = useState<Attempt | null>(() => { try { return readSiteAttempt(sessionStorage.getItem(storageKey)) } catch { return null } })
  const current = useRef(attempt)
  const [notice, setNotice] = useState('')
  const [failure, setFailure] = useState('')
  const inspect = useQuery({ queryKey: ['site-operation', project, site, attempt?.id], queryFn: ({ signal }) => getJson<SiteOperation>(`${sitePath(project, site)}/authoring/${attempt!.id}`, signal), enabled: !!attempt, retry: false, refetchInterval: 2_000 })
  const finish = (result: SiteOperation) => {
    if (result.id !== current.current?.id || result.status === 'pending') return
    setFailure(result.status === 'conflict' ? result.error || 'The live code changed. Review it and try again.' : '')
    setNotice(result.status === 'succeeded' ? current.current.kind === 'snapshot' ? 'Snapshot saved.' : 'Snapshot restored. The restored code is live.' : '')
    current.current = null; setAttempt(null)
    try { sessionStorage.removeItem(storageKey) } catch { /* Memory still retains current state. */ }
    void client.invalidateQueries({ queryKey: siteKeys.detail(project, site) })
    void client.invalidateQueries({ queryKey: siteKeys.snapshots(project, site) })
  }
  // A lost POST reply is resolved by inspecting its durable operation identity.
  useEffect(() => { if (inspect.data) finish(inspect.data) })
  const mutation = useMutation({ mutationFn: (a: Attempt) => postJson<SiteOperation>(`${sitePath(project, site)}/snapshots${a.kind === 'restore' ? `/${a.snapshot}/restore` : ''}`, a.kind === 'snapshot' ? { id: a.id, name: a.name } : { id: a.id }, a.id), retry: false, onSuccess: finish, onError: (error, a) => {
    // An explicit rejection is safe to correct. Ambiguous transport/server
    // failures keep the same operation identity available for inspection/retry.
    if (current.current?.id !== a.id) return
    setFailure(error.message)
    if (error instanceof ApiError && error.status >= 400 && error.status < 500 && ![408, 429].includes(error.status)) {
      current.current = null; setAttempt(null)
      try { sessionStorage.removeItem(storageKey) } catch { /* No saved state to clear. */ }
    }
  } })
  const send = (a: Attempt) => {
    if (current.current || mutation.isPending) return
    current.current = a; setAttempt(a); setFailure(''); setNotice('')
    try { sessionStorage.setItem(storageKey, JSON.stringify(a)) } catch { /* Retry stays available in memory. */ }
    mutation.mutate(a)
  }
  return { busy: !!attempt, submitting: mutation.isPending, notice, error: failure,
    snapshot: (name: string) => send({ id: crypto.randomUUID(), kind: 'snapshot', name: name.trim() }),
    restore: (snapshot: string) => send({ id: crypto.randomUUID(), kind: 'restore', snapshot }),
    retry: () => { if (current.current && !mutation.isPending) mutation.mutate(current.current) },
  }
}
