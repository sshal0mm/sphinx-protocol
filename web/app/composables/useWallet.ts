import type { NearConnector } from '@hot-labs/near-connect'

// One connector for the whole app, created in the browser on first use.
let connector: NearConnector | null = null
let ready: Promise<NearConnector> | null = null

const accountId = ref<string | null>(null)
const busy = ref(false)

const TGAS = 1_000_000_000_000n

// Wallets pass transactions to their signing window with postMessage, which
// cannot copy a page's reactive state (a Vue proxy): hand them plain JSON.
// Every value in a NEAR transaction is plain JSON (amounts are strings).
const plain = <T>(value: T): T => JSON.parse(JSON.stringify(value)) as T

async function load(network: 'mainnet' | 'testnet'): Promise<NearConnector> {
  if (connector) return connector
  ready ??= (async () => {
    const { NearConnector } = await import('@hot-labs/near-connect')
    const c = new NearConnector({
      network,
      autoConnect: true,
      footerBranding: null
    })
    c.on('wallet:signIn', (e) => {
      accountId.value = e.accounts[0]?.accountId ?? null
    })
    c.on('wallet:signOut', () => {
      accountId.value = null
    })
    // A wallet connected in an earlier visit.
    try {
      const { accounts } = await c.getConnectedWallet()
      accountId.value = accounts[0]?.accountId ?? null
    } catch {
      accountId.value = null
    }
    connector = c
    return c
  })()
  return ready
}

/** The visitor's NEAR wallet: connect, disconnect, and calls to the escrow. */
export function useWallet() {
  const { network, contract } = useNetwork()
  const net = network === 'mainnet' ? 'mainnet' : 'testnet'

  onMounted(() => {
    void load(net)
  })

  async function connect() {
    busy.value = true
    try {
      const c = await load(net)
      await c.connect()
    } finally {
      busy.value = false
    }
  }

  async function disconnect() {
    const c = await load(net)
    await c.disconnect()
    accountId.value = null
  }

  /**
   * Calls an escrow method from the connected account, attaching `deposit`
   * yoctoNEAR. Returns the transaction hash and the method's JSON result.
   */
  async function call(method: string, args: object, deposit: bigint, tgas = 80n) {
    const c = await load(net)
    const wallet = await c.wallet()
    const outcome = await wallet.signAndSendTransaction({
      receiverId: contract,
      actions: [{
        type: 'FunctionCall',
        params: { methodName: method, args: plain(args), gas: (tgas * TGAS).toString(), deposit: deposit.toString() }
      }]
    })
    const status = outcome.status as { SuccessValue?: string, Failure?: unknown }
    if (status && 'Failure' in status && status.Failure) {
      throw new Error(JSON.stringify(status.Failure))
    }
    let result: unknown = null
    if (status?.SuccessValue) {
      try {
        result = JSON.parse(atob(status.SuccessValue))
      } catch { /* the method returned nothing */ }
    }
    return { txHash: outcome.transaction.hash as string, result }
  }

  /**
   * Signs and sends `transactions` (to any receivers) from the connected
   * account, in order. Returns each transaction's hash; throws if one failed.
   */
  async function sendTransactions(transactions: { receiverId: string, actions: unknown[] }[]): Promise<string[]> {
    const c = await load(net)
    const wallet = await c.wallet()
    const outcomes = await wallet.signAndSendTransactions({
      transactions: plain(transactions) as Parameters<typeof wallet.signAndSendTransactions>[0]['transactions']
    })
    return outcomes.map((o) => {
      const status = o.status as { Failure?: unknown }
      if (status && 'Failure' in status && status.Failure) {
        throw new Error(JSON.stringify(status.Failure))
      }
      return o.transaction.hash as string
    })
  }

  /**
   * Reads stored code the connected account may see (its own submissions,
   * or the winning code of a job it paid for), proving the account with a
   * NEP-413 signed message. Returns null when the API refuses.
   */
  async function readCode(hash: string): Promise<string | null> {
    const header = await signAuth(`code:${hash}`)
    try {
      return await $fetch<string>(`/v1/code/${hash}`, { responseType: 'text', headers: { 'x-sphinx-auth': header } })
    } catch {
      return null
    }
  }

  /**
   * Signs `sphinx:<subject>:<now>` with NEP-413 and returns the
   * `x-sphinx-auth` value, proving the connected account. Nothing is sent
   * on chain and nothing is paid.
   */
  async function signAuth(subject: string): Promise<string> {
    const c = await load(net)
    const wallet = await c.wallet()
    const nonce = crypto.getRandomValues(new Uint8Array(32))
    const message = `sphinx:${subject}:${Date.now()}`
    const signed = await wallet.signMessage({ message, recipient: 'sphinx-api', nonce })
    return btoa(JSON.stringify({
      account_id: signed.accountId,
      public_key: signed.publicKey,
      signature: signed.signature,
      message,
      nonce: btoa(String.fromCharCode(...nonce)),
      recipient: 'sphinx-api'
    }))
  }

  return { accountId: readonly(accountId), busy: readonly(busy), connect, disconnect, call, sendTransactions, readCode, signAuth }
}
