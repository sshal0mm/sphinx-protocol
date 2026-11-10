# Job: maximum drawdown of an equity curve

Write `function maxDrawdown(equity)`. `equity` is an array of positive integers: account value at the end of each period.

The drawdown at index `i` is `(running peak − equity[i]) / running peak`, where the running peak is the highest value at or before `i`. Return the largest one:

`{ "dd_bps": <integer>, "peak": <index>, "trough": <index> }`

- `dd_bps` is the drawdown in basis points, rounded down (a 12.345% drawdown is 1234).
- `peak` is the index of the running peak, and `trough` the index where the largest drawdown happens. When two drawdowns are equally large (compare the fractions exactly, not the rounded basis points), keep the earlier trough. When the curve sets a new high equal to the old one, the peak stays at the earlier index.
- With no drawdown at all (never falls, or fewer than two values), return `{ "dd_bps": 0, "peak": null, "trough": null }`.

Visible example:

```
maxDrawdown([100, 120, 90, 130, 104, 140]) → {"dd_bps":2500,"peak":1,"trough":2}
```

(120 to 90 is 25%; 130 to 104 is 20%.)

The full suite (hidden) has 16 tests.
