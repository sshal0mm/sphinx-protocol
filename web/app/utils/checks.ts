// What the site lets a wallet sign. The server proposes every transaction;
// these checks refuse anything but what the page showed, so a server that
// went wrong (or was taken over) cannot ask a wallet for more. Each check
// throws, and nothing is signed, unless the transactions are exactly the
// expected ones.

/** A transaction as the wallet connector signs it. */
export interface WalletTx {
  receiverId: string
  actions: unknown[]
}

interface Action {
  type: string
  params: { methodName?: string, args?: Record<string, unknown>, deposit?: string }
}

const YOCTO = 10n ** 24n

/** NEAR in yoctoNEAR from a decimal string, exactly, or null. */
export function toYocto(near: string): bigint | null {
  const m = near.trim().match(/^(\d+)(?:\.(\d{1,24}))?$/)
  if (!m) return null
  return BigInt(m[1]!) * YOCTO + BigInt((m[2] ?? '').padEnd(24, '0'))
}

const refused = (what = 'a transaction') =>
  new Error(`The server asked for ${what} this page did not show, nothing was signed`)

/** The one action of a one-action transaction. */
function only(tx: WalletTx | undefined): Action {
  const actions = (tx?.actions ?? []) as Action[]
  if (actions.length !== 1 || !actions[0]) throw refused()
  return actions[0]
}

const amount = (v: unknown) => {
  try {
    return BigInt(String(v ?? 'x'))
  } catch {
    return -1n
  }
}

/** Caps on what a payment may add beyond the reward the buyer typed: the
 * escrow's job storage deposit (0.025 NEAR today) and the drafting fee
 * (0.05 NEAR today), with headroom, so a server reporting more is refused. */
export const MAX_JOB_STORAGE = 50_000_000_000_000_000_000_000n
export const MAX_DRAFTING_FEE = 100_000_000_000_000_000_000_000n

/**
 * Posting a drafted job: exactly two transactions, the drafting fee as a
 * plain transfer to the escrow's verifier, and `post_job` to the escrow with
 * the reward shown and a deposit of that reward plus the escrow's storage
 * deposit. The page sends the fee first and posts only once it went through.
 */
export function checkPayment(txs: WalletTx[], want: { contract: string, verifier: string, reward: bigint, storage: bigint, fee: bigint }) {
  if (want.storage < 0n || want.storage > MAX_JOB_STORAGE || want.fee < 0n || want.fee > MAX_DRAFTING_FEE) throw refused('a payment')
  const fee = txs.find(t => t.receiverId === want.verifier)
  const post = txs.find(t => t.receiverId === want.contract)
  if (txs.length !== 2 || !post || !fee || post === fee) throw refused('a payment')
  checkFee([fee], want)
  const p = only(post)
  if (p.type !== 'FunctionCall' || p.params.methodName !== 'post_job'
    || amount(p.params.args?.reward) !== want.reward
    || amount(p.params.deposit) !== want.reward + want.storage) throw refused('a payment')
}

/** The drafting fee alone: one plain transfer of exactly the fee to the verifier. */
export function checkFee(txs: WalletTx[], want: { verifier: string, fee: bigint }) {
  const [fee, ...rest] = txs
  if (!fee || rest.length || want.fee < 0n || want.fee > MAX_DRAFTING_FEE) throw refused('a payment')
  const f = only(fee)
  if (fee.receiverId !== want.verifier || f.type !== 'Transfer' || amount(f.params.deposit) !== want.fee) throw refused('a payment')
}

/**
 * Storing a drafted job's hidden tests: `commit_suite` to the escrow with no
 * deposit, then `store_secrets` to the enclave's keystore.
 */
export function checkStore(txs: WalletTx[], want: { contract: string, keystore: string }) {
  const [commit, store] = txs
  if (txs.length !== 2 || !commit || !store) throw refused()
  const c = only(commit)
  const s = only(store)
  const ok = commit.receiverId === want.contract && c.params.methodName === 'commit_suite' && c.params.deposit === '0'
    && store.receiverId === want.keystore && s.params.methodName === 'store_secrets'
  if (!ok) throw refused()
}

/** The most `wrap.near` may take to register the deposit address (NEP-145). */
export const WRAP_REGISTRATION = 1_250_000_000_000_000_000_000n

/**
 * Withdrawing to another chain: one transaction to `wrap.near` with four
 * actions, which succeed or fail together: register the user's own account
 * (so wrapping is never charged for it), register the 1Click deposit
 * address, wrap exactly the quoted NEAR, and send exactly that much wNEAR to
 * the deposit address.
 */
export function checkWithdrawal(txs: WalletTx[], want: { account: string, depositAddress: string, near: bigint }) {
  const [tx, ...rest] = txs
  const a = (tx?.actions ?? []) as Action[]
  const registers = (x: Action | undefined, who: string) => x?.params.methodName === 'storage_deposit'
    && x.params.args?.account_id === who
    && amount(x.params.deposit) >= 0n && amount(x.params.deposit) <= WRAP_REGISTRATION
  const ok = tx && rest.length === 0 && tx.receiverId === 'wrap.near' && a.length === 4
    && a.every(x => x.type === 'FunctionCall')
    && registers(a[0], want.account) && registers(a[1], want.depositAddress)
    && a[2]!.params.methodName === 'near_deposit' && amount(a[2]!.params.deposit) === want.near
    && a[3]!.params.methodName === 'ft_transfer' && a[3]!.params.args?.receiver_id === want.depositAddress
    && amount(a[3]!.params.args?.amount) === want.near && a[3]!.params.deposit === '1'
  if (!ok) throw refused()
}

/** The only methods a connector key may call. */
export const CONNECTOR_METHODS = ['submit_from_balance', 'withdraw_balance', 'withdraw', 'reclaim_bond', 'settle', 'register_agent', 'unregister_agent']
/** The gas allowance a connector key gets (0.25 NEAR). */
export const CONNECTOR_ALLOWANCE = '250000000000000000000000'

/**
 * A connector key the server proposes: limited to the escrow, to a non-empty
 * subset of the connector methods, with exactly the stated allowance.
 */
export function connectorKeyOk(s: { contract?: unknown, allowance_yocto?: unknown, method_names?: unknown, claim?: unknown }, contract: string): boolean {
  const methods = s.method_names
  return s.contract === contract && s.allowance_yocto === CONNECTOR_ALLOWANCE && typeof s.claim === 'string'
    && Array.isArray(methods) && methods.length > 0 && methods.every(m => CONNECTOR_METHODS.includes(m))
}

/**
 * What 1Click itself says about a deposit address (GET /v0/status from
 * 1click.chaindefuser.com, read by the browser, not through Sphinx): the
 * request the quote was made for.
 */
export interface OneClickRecord {
  quoteResponse?: {
    quoteRequest?: {
      recipient?: string
      refundTo?: string
      amount?: string
      swapType?: string
      originAsset?: string
      destinationAsset?: string
    }
    quote?: { depositAddress?: string, amountIn?: string }
  }
}

const refusedAddress = () =>
  new Error('NEAR Intents does not confirm this deposit address for your request, nothing was sent')

/**
 * Paying from another chain: 1Click's own record of the deposit address
 * delivers exactly `near` to the buyer's account and refunds to it, so a
 * server that went wrong cannot show an address that pays anyone else.
 */
export function checkFundRecord(record: OneClickRecord, want: { account: string, near: bigint, depositAddress: string, amountIn: string }) {
  const r = record.quoteResponse?.quoteRequest
  const q = record.quoteResponse?.quote
  const ok = r && q && q.depositAddress?.toLowerCase() === want.depositAddress.toLowerCase()
    && r.swapType === 'EXACT_OUTPUT' && r.destinationAsset === 'nep141:wrap.near'
    && r.recipient === want.account && r.refundTo === want.account
    && amount(r.amount) === want.near && q.amountIn === want.amountIn
  if (!ok) throw refusedAddress()
}

/**
 * Withdrawing to another chain: 1Click's own record sends exactly `near`
 * of wNEAR to the recipient the user typed, and refunds to their account.
 */
export function checkWithdrawRecord(record: OneClickRecord, want: { account: string, near: bigint, recipient: string, depositAddress: string }) {
  const r = record.quoteResponse?.quoteRequest
  const q = record.quoteResponse?.quote
  const ok = r && q && q.depositAddress === want.depositAddress
    && r.swapType === 'EXACT_INPUT' && r.originAsset === 'nep141:wrap.near'
    && r.recipient?.toLowerCase() === want.recipient.toLowerCase() && r.refundTo === want.account
    && amount(r.amount) === want.near
  if (!ok) throw refusedAddress()
}
