// /mainnet/v1/* → the mainnet sphinx-api, whatever the visitor's toggle says.
// Mainnet jobs record their links under this prefix.
export default defineEventHandler(event => proxyApi(event, 'mainnet', getRouterParam(event, 'path') ?? ''))
