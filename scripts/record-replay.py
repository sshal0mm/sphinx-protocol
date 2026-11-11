#!/usr/bin/env python3
"""Records one paid job, step by step, from the chain, for the replay page.

    scripts/record-replay.py <deployments/runs/....json> > deployments/replays/<name>.json

From the job's recorded run it takes the contract, the buyer, the winning
agent and the relayer, then reads their transactions from FastNEAR's
transaction index around the job's lifetime, keeps the ones that belong to
this job (posting it, its hidden tests, its enclave runs, the on-chain
proof checks, the submission and the signed results), and for each one the
time its block was produced. Enclave runs also get the time their result
came back (the last receipt of the run's transaction). No keys needed.
"""

import json
import sys
import time
import urllib.request

run = json.load(open(sys.argv[1]))
mainnet = run["network"] == "mainnet"
TX = "https://tx.main.fastnear.com" if mainnet else "https://tx.test.fastnear.com"
RPC = "https://archival-rpc.mainnet.fastnear.com" if mainnet else "https://archival-rpc.testnet.fastnear.com"
EXPLORER = "https://nearblocks.io/txns/" if mainnet else "https://testnet.nearblocks.io/txns/"
contract, job = run["contract"], run["job"]
job_id = job["id"]
winner = next(s for s in run["submissions"] if s["status"] == "Passed")
start = int(job["created_at_ns"]) - 120 * 10**9
end = int(winner["settled_at_ns"]) + 120 * 10**9
relayer = run["results"][0]["signer"]


def post(url, body):
    for attempt in range(6):
        try:
            req = urllib.request.Request(url, data=json.dumps(body).encode(),
                                         headers={"content-type": "application/json"})
            return json.load(urllib.request.urlopen(req, timeout=30))
        except Exception:  # rate limits and blips: wait and try again
            time.sleep(2 + 3 * attempt)
    raise SystemExit(f"could not read {url}")


def account_txs(account):
    out = post(TX + "/v0/account", {"account_id": account}).get("account_txs", [])
    return [t["transaction_hash"] for t in out
            if t.get("is_signer") and start <= int(t["tx_block_timestamp"]) <= end]


def details(hashes):
    found = []
    for i in range(0, len(hashes), 20):
        found += post(TX + "/v0/transactions", {"tx_hashes": hashes[i:i + 20]})["transactions"]
    return found


def block_time(block_hash):
    r = post(RPC, {"jsonrpc": "2.0", "id": 1, "method": "block", "params": {"block_id": block_hash}})
    return int(r["result"]["header"]["timestamp"])


def last_receipt_time(tx_hash, signer):
    r = post(RPC, {"jsonrpc": "2.0", "id": 1, "method": "EXPERIMENTAL_tx_status",
                   "params": {"tx_hash": tx_hash, "sender_account_id": signer, "wait_until": "FINAL"}})
    outcomes = r["result"]["receipts_outcome"]
    return max(block_time(o["block_hash"]) for o in outcomes) if outcomes else None


def call_of(tx):
    t = tx["transaction"]
    for a in t["actions"]:
        fc = a.get("FunctionCall") if isinstance(a, dict) else None
        if fc:
            try:
                import base64
                args = json.loads(base64.b64decode(fc["args"]))
            except Exception:
                args = {}
            return t["receiver_id"], fc["method_name"], args
    return t["receiver_id"], None, {}


def mentions_job(args):
    text = json.dumps(args)
    return (f'"job_id": "{job_id}"' in text or f'"job_id": {job_id}' in text
            or f'"job_id":"{job_id}"' in text)


steps = []
accounts = [job["buyer"], winner["agent"], relayer]
for account in accounts:
    for tx in details(account_txs(account)):
        receiver, method, args = call_of(tx)
        t = tx["transaction"]
        at = int(tx["execution_outcome"]["block_timestamp"]) if "execution_outcome" in tx and "block_timestamp" in tx["execution_outcome"] else None
        if at is None:
            at = block_time(tx["execution_outcome"]["block_hash"])
        step = {"tx": t["hash"], "signer": t["signer_id"], "receiver": receiver, "method": method, "at_ns": str(at)}
        if receiver == contract and method in ("post_job", "commit_suite", "submit", "confirm_run",
                                               "record_suite_check", "record_score"):
            if method == "post_job" or mentions_job(args):
                if method == "submit" and str(args.get("job_id")) != str(job_id):
                    continue
                if method == "confirm_run":
                    step["for"] = "submission" if args.get("sub_id") is not None else "tests"
                if method == "record_score":
                    sc = args.get("score", {})
                    step["passed"], step["total"] = sc.get("passed"), sc.get("total")
                steps.append(step)
        elif method == "store_secrets" and f"sphinx-job-{job_id}" in json.dumps(args):
            steps.append(step)
        elif method == "request_execution" and account == relayer:
            step["done_ns"] = str(last_receipt_time(t["hash"], t["signer_id"]))
            steps.append(step)

steps.sort(key=lambda s: int(s["at_ns"]))
# The post_job that created this job is the one closest to its creation time.
posts = [s for s in steps if s["method"] == "post_job"]
if posts:
    keep = min(posts, key=lambda s: abs(int(s["at_ns"]) - int(job["created_at_ns"])))
    steps = [s for s in steps if s["method"] != "post_job" or s is keep]
# Enclave runs between the job's post and its payout belong to it: one for
# the tests, one per graded submission.
first = int(steps[0]["at_ns"]) if steps else start
last_score = max((int(s["at_ns"]) for s in steps if s["method"] == "record_score"), default=end)
steps = [s for s in steps if s["method"] != "request_execution" or first <= int(s["at_ns"]) <= last_score]

print(json.dumps({
    "schema": "sphinx.replay/1",
    "network": run["network"],
    "contract": contract,
    "job_id": job_id,
    "title": job.get("title") or run.get("title"),
    "buyer": job["buyer"],
    "agent": winner["agent"],
    "reward": job["reward"],
    "explorer": EXPLORER,
    "run_record": sys.argv[1].split("/")[-1],
    "steps": steps,
}, indent=1))
