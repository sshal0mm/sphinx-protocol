import { describe, expect, it } from 'vitest'
import { checkPayment, checkStore, checkWithdrawal, connectorKeyOk, toYocto, CONNECTOR_ALLOWANCE, type WalletTx } from './checks'

const N = 10n ** 24n
const call = (methodName: string, args: Record<string, unknown>, deposit: string) =>
  ({ type: 'FunctionCall', params: { methodName, args, gas: '30000000000000', deposit } })

describe('toYocto', () => {
  it('parses decimals exactly and refuses anything else', () => {
    expect(toYocto('0.5')).toBe(N / 2n)
    expect(toYocto('12')).toBe(12n * N)
    expect(toYocto('0.000000000000000000000001')).toBe(1n)
    expect(toYocto('1e3')).toBeNull()
    expect(toYocto('-1')).toBeNull()
  })
})

describe('checkPayment', () => {
  const want = { contract: 'escrow.near', verifier: 'v.near', reward: N / 2n, storage: N / 40n, fee: N / 20n }
  const good = (): WalletTx[] => [
    { receiverId: 'escrow.near', actions: [call('post_job', { reward: (N / 2n).toString() }, (N / 2n + N / 40n).toString())] },
    { receiverId: 'v.near', actions: [{ type: 'Transfer', params: { deposit: (N / 20n).toString() } }] }
  ]
  it('accepts exactly the payment shown', () => {
    expect(() => checkPayment(good(), want)).not.toThrow()
  })
  it('refuses a bigger deposit, another receiver, an extra transaction or an extra action', () => {
    const more = good()
    ;(more[0]!.actions[0] as { params: { deposit: string } }).params.deposit = N.toString()
    expect(() => checkPayment(more, want)).toThrow()
    const elsewhere = good()
    elsewhere[1]!.receiverId = 'thief.near'
    expect(() => checkPayment(elsewhere, want)).toThrow()
    expect(() => checkPayment([...good(), good()[1]!], want)).toThrow()
    const extra = good()
    extra[0]!.actions.push({ type: 'Transfer', params: { deposit: '1' } })
    expect(() => checkPayment(extra, want)).toThrow()
  })
})

describe('checkStore', () => {
  const want = { contract: 'escrow.near', keystore: 'outlayer.near' }
  it('accepts commit then store, and refuses a deposit on commit', () => {
    const txs: WalletTx[] = [
      { receiverId: 'escrow.near', actions: [call('commit_suite', {}, '0')] },
      { receiverId: 'outlayer.near', actions: [call('store_secrets', {}, '100')] }
    ]
    expect(() => checkStore(txs, want)).not.toThrow()
    ;(txs[0]!.actions[0] as { params: { deposit: string } }).params.deposit = '5'
    expect(() => checkStore(txs, want)).toThrow()
  })
})

describe('checkWithdrawal', () => {
  const dep = 'd6'.repeat(32)
  const want = { depositAddress: dep, near: N / 2n }
  const good = (): WalletTx[] => [{
    receiverId: 'wrap.near',
    actions: [
      call('storage_deposit', { account_id: dep, registration_only: true }, '1250000000000000000000'),
      call('near_deposit', {}, (N / 2n).toString()),
      call('ft_transfer', { receiver_id: dep, amount: (N / 2n).toString() }, '1')
    ]
  }]
  it('accepts exactly the quoted withdrawal', () => {
    expect(() => checkWithdrawal(good(), want)).not.toThrow()
  })
  it('refuses another recipient, a bigger amount, or another contract', () => {
    const elsewhere = good()
    ;(elsewhere[0]!.actions[2] as { params: { args: { receiver_id: string } } }).params.args.receiver_id = 'thief.near'
    expect(() => checkWithdrawal(elsewhere, want)).toThrow()
    const more = good()
    ;(more[0]!.actions[1] as { params: { deposit: string } }).params.deposit = N.toString()
    expect(() => checkWithdrawal(more, want)).toThrow()
    const other = good()
    other[0]!.receiverId = 'evil.near'
    expect(() => checkWithdrawal(other, want)).toThrow()
  })
})

describe('connectorKeyOk', () => {
  const s = { contract: 'escrow.near', allowance_yocto: CONNECTOR_ALLOWANCE, method_names: ['submit_from_balance'], claim: 'c' }
  it('accepts a key limited to the escrow and connector methods only', () => {
    expect(connectorKeyOk(s, 'escrow.near')).toBe(true)
    expect(connectorKeyOk({ ...s, method_names: ['submit_from_balance', 'set_grader_key'] }, 'escrow.near')).toBe(false)
    expect(connectorKeyOk({ ...s, method_names: [] }, 'escrow.near')).toBe(false)
    expect(connectorKeyOk({ ...s, allowance_yocto: '1' + CONNECTOR_ALLOWANCE }, 'escrow.near')).toBe(false)
    expect(connectorKeyOk(s, 'other.near')).toBe(false)
  })
})
