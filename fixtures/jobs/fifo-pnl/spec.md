# Job: realized PnL from a list of fills (FIFO)

Write `function fifoPnl(fills)` for one instrument. Each fill is
`{ "side": "buy" | "sell", "qty": <positive integer>, "price_cents": <integer>, "fee_cents": <integer, may be missing> }`, in time order.

Match closing quantity against open lots first in, first out. A position can go long, flat and short, and one fill can close a position and open the opposite one (selling 15 while long 10 closes the 10 and opens a 5 short at the fill's price).

- Closing a long lot earns `qty × (sell price − lot price)`; closing a short lot earns `qty × (lot price − buy price)`.
- Every fee is subtracted from realized PnL, whether the fill opens or closes.

Return `{ "realized_cents": <integer>, "position": <signed integer, + long, − short>, "avg_open_price_cents": <integer or null> }`, where the average open price is the quantity-weighted price of the lots still open, rounded to the nearest cent (halves round up), and `null` when flat.

Visible example:

```
fifoPnl([
  {"side":"buy","qty":10,"price_cents":10000,"fee_cents":100},
  {"side":"buy","qty":10,"price_cents":11000,"fee_cents":100},
  {"side":"sell","qty":15,"price_cents":12000,"fee_cents":150}
])
→ {"realized_cents":24650,"position":5,"avg_open_price_cents":11000}
```

The full suite (hidden) has 18 tests.
