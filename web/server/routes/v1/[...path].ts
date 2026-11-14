// Same-origin /v1/* → the sphinx-api of the visitor's network (its cookie, else
// the site default), so the API has no public route of its own. API addresses
// are plain paths, so no query string is passed on. A stored file asked for by
// hash is looked up on the other network too, so older links keep working.
export default defineEventHandler((event) => {
  const path = getRouterParam(event, 'path') ?? ''
  const network = networkOf(event)
  return path.startsWith('code/') ? codeFromEither(event, network, path) : proxyApi(event, network, path)
})
