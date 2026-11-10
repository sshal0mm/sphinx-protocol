# 0001. Signed result format

**Status:** accepted

**Context.** The grader, the contract and the CLI must agree byte for byte on what is signed. Earlier drafts built the message in several places, and any drift showed up only as an invalid signature.

**Decision.** One definition in `sphinx-protocol`: `SuiteCheck::signing_message` and `Score::signing_message`, colon-separated ASCII with a `sphinx:v1:<kind>:` prefix. Every field is an account id, integer, bool, runtime id or lowercase hex, so no field can contain the separator. Golden vectors in `fixtures/golden/messages.json` are checked by the protocol, contract and CLI tests. The contract receives the structured result, compares each field with its state, rebuilds the message itself, and only then verifies the signature.

**Consequences.** A format change breaks the golden test first. Two message kinds cannot be confused. Callers never build signing strings.
