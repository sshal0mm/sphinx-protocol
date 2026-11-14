// /attestation/by-tx/<hash> or /attestation/by-call/<id>: fetches that OutLayer
// run attestation for the verify page. Both parts are checked against fixed
// patterns, and only OutLayer's attestation API is ever contacted.
export default defineEventHandler(async (event) => {
  const kind = getRouterParam(event, 'kind') ?? ''
  const id = getRouterParam(event, 'id') ?? ''
  if (!/^by-(tx|call)$/.test(kind) || !/^[A-Za-z0-9]{1,64}$/.test(id)) {
    throw createError({ statusCode: 404 })
  }
  const network = networkOf(event)
  const host = network === 'mainnet' ? 'https://api.outlayer.ai' : 'https://testnet-api.outlayer.ai'
  const a = await $fetch<Record<string, unknown>>(`${host}/attestations/${kind}/${id}`, { timeout: 10_000 })
  // Only the fields the page compares; the quote itself is linked, not copied.
  return {
    caller_account_id: a.caller_account_id,
    executed_wasm_sha256: a.executed_wasm_sha256,
    wasm_hash: a.wasm_hash,
    worker_measurement: a.worker_measurement,
    transaction_hash: a.transaction_hash,
    has_quote: Boolean(a.tdx_quote ?? a.quote)
  }
})
