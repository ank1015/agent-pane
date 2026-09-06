/* eslint-disable no-control-regex -- Reject control characters in untrusted endpoint paths. */
export type SiteCall = { id: string; endpoint: string; input?: unknown }
export function validSiteCall(value: unknown): value is SiteCall {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false
  const v = value as Record<string, unknown>
  try {
    return typeof v.id === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(v.id) && typeof v.endpoint === 'string' && v.endpoint.startsWith('/') && !v.endpoint.startsWith('//') && v.endpoint.length <= 2048 && !/[?#\x00-\x1f\x7f]/.test(v.endpoint) && Object.keys(v).every(k => ['id', 'endpoint', 'input'].includes(k)) && new TextEncoder().encode(JSON.stringify(v)).length <= 128 * 1024
  } catch { return false }
}
export function backendResult(record: unknown): unknown {
  const v = record as { status?: string; error_code?: string; response?: { status: number; body: unknown } }
  if (v?.status !== 'succeeded' || !v.response) throw new Error(`Backend execution failed (${v?.error_code || v?.status || 'invalid response'}).`)
  if (v.response.status < 200 || v.response.status >= 300) {
    const body = v.response.body as { error?: string; message?: string } | null
    throw new Error(typeof body?.error === 'string' ? body.error : typeof body?.message === 'string' ? body.message : `Backend returned ${v.response.status}.`)
  }
  return v.response.body
}
