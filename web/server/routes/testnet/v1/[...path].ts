// /testnet/v1/* → the testnet sphinx-api, whatever the visitor's toggle says.
export default defineEventHandler(event => proxyApi(event, 'testnet', getRouterParam(event, 'path') ?? ''))
