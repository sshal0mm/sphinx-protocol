import type { H3Event } from 'h3'

// /newcomer/* → sphinx-house on the internal network. Plain paths only, and
// the proxy names the visitor so the house can apply its per-visitor caps.
export function proxyToHouse(event: H3Event, path: string) {
  const { house } = servicesOf(event)
  if (!house) throw createError({ statusCode: 404 })
  const client = getRequestHeader(event, 'cf-connecting-ip')
    ?? getRequestIP(event)
    ?? 'unknown'
  const headers = new Headers()
  const type = getRequestHeader(event, 'content-type')
  if (type) headers.set('content-type', type)
  headers.set('x-sphinx-client', client)
  return proxyRequest(event, `${house}/newcomer${path}`, { headers })
}
