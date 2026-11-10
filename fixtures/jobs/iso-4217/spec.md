# Job: currency reference data (ISO 4217)

A research job: the answers are facts, and the hidden tests are an answer key.

Write `function currency(code)` that returns, for an active ISO 4217 currency code, `{ "code": <upper-case code>, "numeric": <three-digit string, with leading zeros>, "minor_units": <integer> }`, as published in the current ISO 4217 list (Table A.1).

- Accept codes in any letter case, with surrounding spaces trimmed (`" kwd "` is KWD).
- Return `null` for anything that is not an active ISO 4217 currency code (withdrawn codes, made-up codes, non-strings).
- The hidden tests use currencies from every continent, including ones whose minor units are 0, 3 or 4.

Visible examples:

```
currency("usd") → {"code":"USD","numeric":"840","minor_units":2}
currency("JPY") → {"code":"JPY","numeric":"392","minor_units":0}
currency("ABC") → null
```

The full suite (hidden) has 24 tests.
