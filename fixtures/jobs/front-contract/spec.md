# Job: which futures contract is the front month?

Write `function frontContract(symbol, date, rollDays)` for the quarterly CME contracts ES (E-mini S&P 500), NQ (E-mini Nasdaq-100), RTY (E-mini Russell 2000) and 6E (Euro FX).

- `date` is a calendar date `"YYYY-MM-DD"`; `rollDays` is an integer from 0 to 60.
- The front contract is the earliest quarterly contract (March, June, September, December) whose **last trading day** is more than `rollDays` calendar days after `date`. With `rollDays` 0, a contract stops being the front on its last trading day.
- Return `{ "code": <string>, "expiry": "YYYY-MM-DD" }`, where `code` is the symbol, the standard CME month code letter, and the last digit of the year (`"ESZ6"` for December 2026), and `expiry` is the last trading day.

Finding the last trading day is part of the job: use each contract's CME rule. Ignore exchange holidays (treat every Monday to Friday as a business day).

Return `null` for any other symbol, a date that is not a real `YYYY-MM-DD` date, or a `rollDays` outside 0 to 60.

Visible example:

```
frontContract("ES", "2026-11-07", 8) → {"code":"ESZ6","expiry":"2026-12-18"}
```

The full suite (hidden) has 20 tests.
