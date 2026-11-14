import type { H3Event } from 'h3'

type Network = 'mainnet' | 'testnet'

/** The internal API address of `network`, or '' when it is not set up. */
export function apiOf(event: H3Event, network: Network): string {
  const services = (useRuntimeConfig(event) as unknown as { services: Record<Network, { api: string }> }).services
  return services[network]?.api ?? ''
}

/** Headers for the API: the visitor named by the proxy, and nothing only the
 * proxy or the relayer may set. */
function apiHeaders(event: H3Event): Headers {
  const client = getRequestHeader(event, 'cf-connecting-ip')
    ?? getRequestIP(event)
    ?? 'unknown'
  const headers = new Headers()
  for (const [name, value] of Object.entries(getRequestHeaders(event))) {
    const lower = name.toLowerCase()
    if (value === undefined || lower === 'x-sphinx-reader' || lower === 'x-sphinx-client' || lower === 'host') continue
    headers.set(lower, value)
  }
  headers.set('x-sphinx-client', client)
  return headers
}

/** Sends `/v1/<path>` to `network`'s API. */
export function proxyApi(event: H3Event, network: Network, path: string) {
  const api = apiOf(event, network)
  if (!api) throw createError({ statusCode: 404, statusMessage: `${network} is not live yet` })
  return proxyRequest(event, `${api}/v1/${path}`, { headers: apiHeaders(event) })
}

/**
 * A stored file by hash (`code/<hash>`) from either network: the chosen one
 * first, then the other. Jobs record links without a network in them, so a
 * link written on one network keeps working whichever one is the default.
 */
export async function codeFromEither(event: H3Event, first: Network, path: string) {
  const second: Network = first === 'mainnet' ? 'testnet' : 'mainnet'
  const other = apiOf(event, second)
  if (event.method !== 'GET' || !other || !apiOf(event, first)) return proxyApi(event, first, path)
  const res = await fetch(`${apiOf(event, first)}/v1/${path}`, { headers: apiHeaders(event) })
  if (res.status !== 404) return sendWebResponse(event, res)
  return proxyApi(event, second, path)
}
