---
title: the relayer
section: network
order: 3
---

The relayer starts enclave runs and posts their signed results. It cannot change or forge a score, but it is the only account that can start a grading run, because the grader's keys are bound to it, so it decides which submissions are graded and when.

## what it does

Each pass reads every active job from the chain and does whatever is due:

- a committed job: run the suite check in the enclave
- a pending submission: run the grader on the submitted code
- a job that ended with submissions still pending: return their bonds

Before paying for an enclave run it checks that the job is still waiting and its hidden suite is stored.

## reading code

The relayer reads submitted code only from the Sphinx code store, by the hash committed on chain, with a 15 second timeout, no redirects and a 64 KiB cap. The grader checks the hash again.

## if it stops

Anyone holding a signed result may post it, but only the relayer's account can produce one. If none runs, no funds are stuck: pending bonds are reclaimable after the grading window, and rewards are refundable after the deadline and the grading window.
