export default defineEventHandler((event) => {
  const path = getRouterParam(event, 'path') ?? ''
  return proxyToHouse(event, `/${path}`)
})
