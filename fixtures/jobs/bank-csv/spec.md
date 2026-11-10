# Job: normalize a bank CSV export

Write `function normalize(csv)` that turns the text of a bank's CSV export into an array of transactions:
`{ "date": "YYYY-MM-DD", "amount_cents": <integer>, "description": <string> }`, in the order the rows appear.

Banks disagree on everything, so handle all of this:

- **Header row first.** Column names are matched case-insensitively after trimming. The date is in `date`, `booking date` or `value date`. The amount is either one signed column (`amount` or `value`) or two columns, `credit` and `debit`: then the amount is credit minus the absolute value of debit, and an empty cell counts as 0. The description is in `description`, `details`, `memo` or `payee`; with no such column it is `""`.
- **Quoting.** Fields may be wrapped in double quotes and then contain commas, newlines and `""` for a literal quote. Lines may end in `\r\n` or `\n`. Blank lines are ignored.
- **Dates.** `YYYY-MM-DD`, `DD/MM/YYYY` or `DD.MM.YYYY`. Leading and trailing spaces are ignored. A date that does not exist (for example 31/04/2026 or 29.02.2025) is invalid.
- **Amounts.** May carry either a sign (`-` or `+`) or parentheses for a negative amount (`(12.50)`), and currency symbols (`$`, `€`, `£`) and spaces anywhere (`$ -45.00` is −4500 cents). The decimal separator is the last `.` or `,` when one or two digits follow it; every other `.`, `,` or `'` is a thousands separator. So `1.234,56` and `1,234.56` and `1'234.56` are all 123456 cents, `12,5` is 1250 cents, and `1,000` is 100000 cents.
- **Bad rows.** A row whose date or amount cannot be read is skipped. Descriptions are trimmed.

Plain JavaScript only: no regular expressions (the grader disables them).

Visible example:

```
normalize("Date,Description,Amount\n2026-03-01,Coffee,-3.50\n02/03/2026,\"Salary, March\",\"2.500,00\"")
→ [ {"date":"2026-03-01","amount_cents":-350,"description":"Coffee"},
    {"date":"2026-03-02","amount_cents":250000,"description":"Salary, March"} ]
```

The full suite (hidden) has 20 tests.
