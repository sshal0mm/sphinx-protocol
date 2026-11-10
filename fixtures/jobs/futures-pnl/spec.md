# Job: futures trade PnL in ticks and dollars

Write `function futuresPnl(symbol, side, entry, exit, contracts)` for one round-trip futures trade.

- `symbol` is one of the CME contracts below; `side` is `"long"` or `"short"`; `entry` and `exit` are prices as **decimal strings** (for example `"5432.25"` or `"1.08455"`); `contracts` is a positive integer.
- Return `{ "ticks": <integer>, "pnl_cents": <integer> }`: the number of ticks gained per contract (negative for a loss) and the total profit in US cents for all contracts, before fees.

| Symbol | Contract | Tick size | Tick value |
|---|---|---|---|
| ES | E-mini S&P 500 | 0.25 | $12.50 |
| NQ | E-mini Nasdaq-100 | 0.25 | $5.00 |
| MES | Micro E-mini S&P 500 | 0.25 | $1.25 |
| MNQ | Micro E-mini Nasdaq-100 | 0.25 | $0.50 |
| CL | Crude oil | 0.01 | $10.00 |
| GC | Gold | 0.10 | $10.00 |
| 6E | Euro FX | 0.00005 | $6.25 |

Return `null` for an unknown symbol, a side other than the two above, a contract count that is not a positive integer, a price that is not a plain decimal string (digits, at most one `.` with 1 to 5 digits after it, an optional leading `-`), or a price that is not a whole number of ticks. Work in integers: binary floating point gets prices like 1.08455 wrong.

Visible examples:

```
futuresPnl("ES", "long", "5432.25", "5440.00", 2)  → {"ticks":31,"pnl_cents":77500}
futuresPnl("CL", "short", "78.40", "78.95", 1)     → {"ticks":-55,"pnl_cents":-55000}
```

The full suite (hidden) has 20 tests.
