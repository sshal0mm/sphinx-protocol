# 0005. Contract upgrade policy

**Status:** accepted

**Context.** Versioned state and migrations add code and risk before there is state worth migrating.

**Decision.** The storage layout is frozen per release. Code-only changes that keep the layout are deployed in place (`SPHINX_UPGRADE=1 scripts/deploy.sh`). Layout changes deploy to a new account. The owner keeps a full-access key during development and says so in the README.

**Consequences.** No migration code. Once the design is stable, the contract account's keys can be removed to make the code immutable.
