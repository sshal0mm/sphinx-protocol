# Job: cross-rate currency conversion in minor units

Write `function convert(rates, from, to, amountMinor)`.

- `rates` maps a currency code to its value in **micro-dollars**: how many millionths of a US dollar one unit of that currency is worth. `{"EUR": 1084200}` means 1 EUR = 1.0842 USD. USD itself is always 1000000, whether or not it appears in `rates`.
- `amountMinor` is an integer amount of `from` in its **minor units** (cents for USD, yen for JPY, fils for KWD), with absolute value at most 1,000,000.
- Return the converted amount as an integer in the minor units of `to`.

The exact result is `amountMinor × rate(from) × 10^minor(to) ÷ (rate(to) × 10^minor(from))`. Round it once, at the end, **half to even** (banker's rounding), and symmetrically for negative amounts. Do not round at an intermediate step.

Minor units follow ISO 4217. The currencies in play are USD, EUR, GBP, CHF, CAD, AUD, NZD, SEK, NOK, MXN, ZAR, TRY, INR, NGN, BRL, SGD, HKD, CNY and PLN (2 decimals); JPY, KRW, ISK and CLP (0 decimals); and KWD, BHD, JOD and OMR (3 decimals).

Return `null` when either currency is not in that list, or a needed rate is missing or not positive. Converting a listed currency to itself returns the amount unchanged, even with no rates.

Visible examples:

```
convert({"EUR":1084200}, "EUR", "USD", 1000)          → 1084     (10.00 EUR = 10.842 USD → 10.84)
convert({"EUR":1084200,"JPY":6667}, "EUR", "JPY", 100) → 163       (1.00 EUR = 162.62… JPY)
```

The full suite (hidden) has 20 tests.
