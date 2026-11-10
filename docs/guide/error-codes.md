---
title: error codes
section: build
order: 6
---

Every failure, from the API, the contract, the grader or the tools, has the same shape. Act on `retry` and `bond_effect` without parsing the message.

```json
{
  "code": "SPHINX_E_JOB_WRONG_STATE",
  "message": "job 7 is Paid; this call needs Open",
  "hint": "read the job for its current status (contract view get_job; MCP sphinx_jobs with job_id)",
  "retry": "never",
  "bond_effect": "none",
  "ctx": { "job_id": "7" }
}
```

| Field | Values |
|---|---|
| `retry` | `never`, `fix_input`, `wait` or `transient` |
| `bond_effect` | `none`, `forfeited`, `returned` or `held` |
| `ctx` | the values involved, as strings |

## http status

| Status | When |
|---|---|
| 404 | the job, submission or code does not exist |
| 413 | code larger than 64 KiB |
| 422 | anything with `retry: fix_input` |
| 409 | anything with `retry: never` or `wait` |
| 429 | rate limited, with `Retry-After` |
| 502 | the NEAR RPC failed |
| 503 | anything else with `retry: transient` |
