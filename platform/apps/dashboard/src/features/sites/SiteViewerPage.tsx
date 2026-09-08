import { useEffect, useRef, useState } from 'react'
import { Link, useParams } from 'react-router-dom'
import { postJson } from '../../lib/api-client'
import { useSite } from './site-queries'
import { backendResult, validSiteCall } from './site-bridge'
import '../../styles/sites.css'
type View = { name: string; url: string; releaseId: string; token: string; expiresAt: number }
export function SiteViewerPage() {
  const { projectId = '', siteId = '' } = useParams()
  const detail = useSite(projectId, siteId)
  if (!detail.data?.resource.active_release_id || detail.data.site.desired_status !== 'ready') return <main className="site-viewer"><div className="site-viewer-error"><p role={detail.isError ? 'alert' : 'status'}>{detail.error?.message || (detail.isPending ? 'Opening site…' : 'This site has no live code available yet.')}</p><Link to={`/projects/${projectId}/sites/${siteId}`}>Back to site</Link></div></main>
  return <SitePreview key={`${projectId}:${siteId}`} projectId={projectId} siteId={siteId} revision={detail.data.resource.active_release_id} />
}
type PreviewProps = { projectId: string; siteId: string; revision: string; embedded?: boolean }
export function SitePreview(props: PreviewProps) {
  return <Preview key={`${props.projectId}:${props.siteId}:${props.revision}`} {...props} />
}
function Preview({ projectId, siteId, embedded = false }: PreviewProps) {
  const [view, setView] = useState<View | null>(null)
  const [error, setError] = useState('')
  const [generation, setGeneration] = useState(0)
  useEffect(() => {
    let current = true
    const previousTitle = document.title
    if (!embedded) document.title = 'Opening site…'
    postJson<View>(`/api/site-view/projects/${encodeURIComponent(projectId)}/sites/${encodeURIComponent(siteId)}/open`, {}).then(v => {
      if (current) { setView(v); if (!embedded) document.title = `${v.name} · Sites` }
    }).catch(e => { if (current) setError(e.message) })
    return () => { current = false; if (!embedded) document.title = previousTitle }
  }, [projectId, siteId, generation, embedded])
  return <main className={embedded ? 'site-viewer site-viewer--embedded' : 'site-viewer'} aria-label={view?.name || 'Site viewer'}>
    {view ? <SiteFrame key={view.token} view={view} onError={setError} /> : !error ? <p className="site-viewer-notice" role="status">Opening site…</p> : null}
    {error ? <div className="site-viewer-error" role="alert"><p>{error}</p><button onClick={() => { setView(null); setError(''); setGeneration(g => g + 1) }}>Reload site</button><Link to={`/projects/${projectId}/sites`}>Back to Sites</Link></div> : null}
  </main>
}
function SiteFrame({ view, onError }: { view: View; onError: (error: string) => void }) {
  const frame = useRef<HTMLIFrameElement>(null)
  const loads = useRef(0)
  const dispose = useRef<() => void>(() => {})
  useEffect(() => {
    let channel: MessageChannel | undefined
    let closed = false
    const pending = new Set<string>()
    const controllers = new Set<AbortController>()
    const seen = new Set<string>()
    const close = () => { closed = true; channel?.port1.close(); controllers.forEach(c => c.abort()); window.removeEventListener('message', connect) }
    const connect = (event: MessageEvent) => {
      if (closed || channel || event.source !== frame.current?.contentWindow || event.origin !== 'null' || event.data?.type !== 'sites:ready:v1') return
      channel = new MessageChannel()
      channel.port1.onmessage = async ({ data }) => {
        const port = channel!.port1
        if (closed) return
        if (!validSiteCall(data)) { if (typeof data?.id === 'string' && data.id.length <= 64) port.postMessage({ id: data.id, error: { message: 'Invalid backend request.' } }); return }
        if (seen.has(data.id)) return
        if (pending.size >= 8 || seen.size >= 2000) { port.postMessage({ id: data.id, error: { message: 'Site request limit reached. Reload the site.' } }); return }
        seen.add(data.id); pending.add(data.id)
        const controller = new AbortController(); controllers.add(controller)
        const timeout = setTimeout(() => controller.abort(), 42000)
        try {
          const response = await fetch('/api/site-view/invoke', { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-Site-View': view.token }, body: JSON.stringify(data), signal: controller.signal })
          const result = await response.json()
          if (!response.ok) throw new Error(result?.error?.message || 'Site request failed.')
          if (!closed) port.postMessage({ id: data.id, result: backendResult(result) })
        } catch (e) { if (!closed) port.postMessage({ id: data.id, error: { message: e instanceof Error ? e.message : 'Site request failed.' } }) }
        finally { clearTimeout(timeout); pending.delete(data.id); controllers.delete(controller) }
      }
      channel.port1.start()
      // The sandbox has an opaque origin, so a literal target origin cannot be
      // used. The exact WindowProxy was checked above; only it receives this port.
      frame.current!.contentWindow!.postMessage({ type: 'sites:connect:v1' }, '*', [channel.port2])
    }
    dispose.current = close
    window.addEventListener('message', connect)
    const expiry = setTimeout(() => { close(); onError('This site view expired. Reload to reconnect.') }, Math.max(0, view.expiresAt - Date.now()))
    const startup = setTimeout(() => { if (!channel) onError('The site did not connect. Check that its content service is available.') }, 15000)
    return () => { clearTimeout(expiry); clearTimeout(startup); close() }
  }, [view, onError])
  return <iframe ref={frame} className="site-viewer-frame" src={view.url} title={view.name} sandbox="allow-scripts" referrerPolicy="no-referrer" onLoad={() => {
    if (++loads.current > 1) { dispose.current(); onError('The site navigated away from its published page. Reload to reconnect.') }
  }} />
}
