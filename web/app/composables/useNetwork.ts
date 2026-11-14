export type Network = 'mainnet' | 'testnet'

/** The cookie that remembers which network the visitor is looking at. */
export const NETWORK_COOKIE = 'sphinx-network'

/**
 * The network this page shows: the visitor's choice when that network is
 * set up, else the site default. Switching reloads the page, so every value
 * here is plain and read once.
 */
export function useNetwork() {
  const { networks, defaultNetwork } = useRuntimeConfig().public
  const ready = (n: string): n is Network => (n === 'mainnet' || n === 'testnet') && Boolean(networks[n]?.contract)
  const choices = (['mainnet', 'testnet'] as const).filter(ready)
  const cookie = useCookie<string | null>(NETWORK_COOKIE, { sameSite: 'lax', maxAge: 60 * 60 * 24 * 365 })
  const fallback: Network = ready(defaultNetwork) ? defaultNetwork : (choices[0] ?? 'testnet')
  const network: Network = cookie.value && ready(cookie.value) ? cookie.value : fallback
  const { contract, explorer, verifier } = networks[network] as { contract: string, explorer: string, verifier: string }

  function switchTo(next: Network) {
    if (next === network || !ready(next)) return
    cookie.value = next
    if (import.meta.client) window.location.reload()
  }

  return { network, contract, explorer, verifier, choices, switchTo }
}
