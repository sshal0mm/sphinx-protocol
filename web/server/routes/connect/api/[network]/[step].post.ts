// /connect/api/<network>/<start|finish|status|mine> → that network's connect server
// (sphinx-mcp --connect), which makes the connector key and, once the
// owner's wallet has added it, hands out the connector address.
export default defineEventHandler(async (event) => {
  const network = getRouterParam(event, 'network')
  const step = getRouterParam(event, 'step')
  if ((network !== 'mainnet' && network !== 'testnet') || !['start', 'finish', 'status', 'mine'].includes(step ?? '')) {
    throw createError({ statusCode: 404 })
  }
  const services = (useRuntimeConfig(event) as unknown as { services: Record<string, { connect: string }> }).services
  const base = services[network]?.connect
  if (!base) throw createError({ statusCode: 404, statusMessage: `${network} connectors are not available yet` })
  const body = await readBody(event)
  const res = await fetch(`${base}/connect/${step}`, {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      // Sign-ups are limited per visitor; the tunnel names the visitor.
      'x-sphinx-client': getRequestHeader(event, 'cf-connecting-ip') ?? getRequestIP(event) ?? 'unknown'
    },
    body: JSON.stringify(body ?? {})
  })
  setResponseStatus(event, res.status)
  return await res.json()
})
