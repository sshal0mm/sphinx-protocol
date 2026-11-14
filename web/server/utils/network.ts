import type { H3Event } from 'h3'

/**
 * The network a request is for, from the visitor's cookie, falling back to
 * the site default; a network whose services are not configured is never
 * chosen.
 */
type Network = 'mainnet' | 'testnet'
interface Config {
  services: Record<Network, { api: string, house: string }>
  public: { defaultNetwork: string, networks: Record<Network, { contract: string }> }
}

export function networkOf(event: H3Event): Network {
  const config = useRuntimeConfig(event) as unknown as Config
  const ready = (n: string | undefined): n is Network =>
    (n === 'mainnet' || n === 'testnet') && Boolean(config.public.networks[n]?.contract) && Boolean(config.services[n]?.api)
  const chosen = getCookie(event, 'sphinx-network')
  if (ready(chosen)) return chosen
  if (ready(config.public.defaultNetwork)) return config.public.defaultNetwork
  return ready('mainnet') ? 'mainnet' : 'testnet'
}

/** Internal addresses of the API and the house for the request's network. */
export function servicesOf(event: H3Event) {
  return (useRuntimeConfig(event) as unknown as Config).services[networkOf(event)]
}
