// NEAR Intents (1Click) through the house: pay with USDC or USDT from
// another chain, or withdraw NEAR to one. Sphinx never holds the money: the
// deposit addresses are 1Click's, the recipients are the user's own.

/** Marks TokenIcon draws: the tokens and the chains they come from. */
export type TokenIconName = 'usdc' | 'usdt' | 'base' | 'arbitrum' | 'ethereum' | 'solana' | 'optimism' | 'polygon' | 'avalanche' | 'near'

export interface IntentsOptions {
  mainnet: boolean
  near_usd: number | null
  chains: { id: string, name: string, tokens: string[] }[]
  app_fee_bps: number
}

export interface FundQuote {
  direction: 'fund'
  chain: string
  chain_name: string
  token: string
  deposit_address: string
  deposit_memo: string | null
  amount_in: string
  amount_in_formatted: string
  amount_in_usd: string
  near_out: string
  recipient: string
  seconds: number
  deadline: string
  app_fee_bps: number
  partner_fee: boolean
}

export interface WithdrawQuote {
  direction: 'withdraw'
  chain: string
  chain_name: string
  token: string
  deposit_address: string
  near_in: string
  recipient: string
  amount_out_formatted: string
  min_amount_out_formatted: string | null
  amount_out_usd: string
  seconds: number
  deadline: string
  app_fee_bps: number
  partner_fee: boolean
  transactions: { receiverId: string, actions: unknown[] }[]
}

export interface SwapStatus {
  status: 'PENDING_DEPOSIT' | 'KNOWN_DEPOSIT_TX' | 'PROCESSING' | 'SUCCESS' | 'INCOMPLETE_DEPOSIT' | 'REFUNDED' | 'FAILED'
  deposited_formatted: string | null
  amount_out_formatted: string | null
  refunded_formatted: string | null
  refund_reason: string | null
  explorer: string
}

/** What each 1Click status means, in the site's words. */
export const SWAP_STEPS: Record<SwapStatus['status'], string> = {
  PENDING_DEPOSIT: 'waiting for your deposit',
  KNOWN_DEPOSIT_TX: 'deposit seen, waiting for it to confirm',
  PROCESSING: 'swapping through NEAR Intents',
  SUCCESS: 'done',
  INCOMPLETE_DEPOSIT: 'the deposit was less than asked, send the rest or it is refunded',
  REFUNDED: 'refunded',
  FAILED: 'the swap failed'
}

export const SWAP_DONE = new Set<SwapStatus['status']>(['SUCCESS', 'REFUNDED', 'FAILED'])

/** Error text from a house refusal. */
export function intentsError(e: unknown, fallback: string): string {
  const data = (e as { data?: { error?: { message?: string } } }).data
  return data?.error?.message ?? fallback
}

export function useIntents() {
  const base = '/newcomer/intents'
  return {
    options: () => $fetch<IntentsOptions>(`${base}/options`),
    fund: (body: { account: string, near: string, chain: string, token: string }) =>
      $fetch<FundQuote>(`${base}/fund`, { method: 'POST', body }),
    withdraw: (body: { account: string, near: string, chain: string, token: string, recipient: string }) =>
      $fetch<WithdrawQuote>(`${base}/withdraw`, { method: 'POST', body }),
    status: (deposit: string) => $fetch<SwapStatus>(`${base}/status/${encodeURIComponent(deposit)}`),
    submit: (body: { deposit_address: string, tx_hash: string, near_sender?: string }) =>
      $fetch(`${base}/submit`, { method: 'POST', body }),
    /** wNEAR held, and NEAR the account can withdraw, in yoctoNEAR. */
    balance: async (account: string) => {
      const b = await $fetch<{ wnear_yocto: string, spendable_yocto: string }>(`${base}/balance/${encodeURIComponent(account)}`)
      return { wnear: BigInt(b.wnear_yocto), spendable: BigInt(b.spendable_yocto) }
    }
  }
}
