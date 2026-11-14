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

/**
 * Posting a drafted job: exactly two transactions, `post_job` to the escrow
 * with the reward shown and a deposit of that reward plus the escrow's
 * storage deposit, and the drafting fee as a plain transfer to the escrow's
 * verifier.
 */
export function checkPayment(txs: WalletTx[], want: { contract: string, verifier: string, reward: bigint, storage: bigint, fee: bigint }) {
  const [post, fee] = txs
  if (txs.length !== 2 || !post || !fee) throw refused('a payment')
  const p = only(post)
  const f = only(fee)
  if (post.receiverId !== want.contract || p.type !== 'FunctionCall' || p.params.methodName !== 'post_job'
    || amount(p.params.args?.reward) !== want.reward
    || amount(p.params.deposit) !== want.reward + want.storage) throw refused('a payment')
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
 * Withdrawing to another chain: one transaction to `wrap.near` with three
 * actions, registering the 1Click deposit address, wrapping exactly the
 * quoted NEAR, and sending exactly that much wNEAR to the deposit address.
 */
export function checkWithdrawal(txs: WalletTx[], want: { depositAddress: string, near: bigint }) {
  const [tx, ...rest] = txs
  const a = (tx?.actions ?? []) as Action[]
  const ok = tx && rest.length === 0 && tx.receiverId === 'wrap.near' && a.length === 3
    && a.every(x => x.type === 'FunctionCall')
    && a[0]!.params.methodName === 'storage_deposit' && a[0]!.params.args?.account_id === want.depositAddress
    && amount(a[0]!.params.deposit) >= 0n && amount(a[0]!.params.deposit) <= WRAP_REGISTRATION
    && a[1]!.params.methodName === 'near_deposit' && amount(a[1]!.params.deposit) === want.near
    && a[2]!.params.methodName === 'ft_transfer' && a[2]!.params.args?.receiver_id === want.depositAddress
    && amount(a[2]!.params.args?.amount) === want.near && a[2]!.params.deposit === '1'
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
