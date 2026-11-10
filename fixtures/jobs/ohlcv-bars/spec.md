# Job: build OHLCV bars from trades

Write `function bars(ticks, intervalMs)`. `ticks` is an array of trades `{ "t": <ms since epoch>, "price": <integer>, "qty": <integer> }` sorted by `t` (several trades may share a timestamp). `intervalMs` is a positive integer.

Return one bar per interval that contains at least one trade, in time order:
`{ "t": <interval start>, "o": open, "h": high, "l": low, "c": close, "v": total qty, "n": number of trades }`.

- The interval start is `t` rounded down to a multiple of `intervalMs`, measured from 0. This holds for negative `t` too (−1 with a 1000 ms interval starts at −1000).
- Open is the first trade's price in the interval and close the last, in input order.
- Intervals with no trades produce no bar (no gap filling).
- An empty input gives `[]`.

Visible example:

```
bars([{"t":1000,"price":100,"qty":2},{"t":1500,"price":105,"qty":1},{"t":2100,"price":99,"qty":4}], 1000)
→ [{"t":1000,"o":100,"h":105,"l":100,"c":105,"v":3,"n":2},
   {"t":2000,"o":99,"h":99,"l":99,"c":99,"v":4,"n":1}]
```

The full suite (hidden) has 16 tests.
