# Hydragent Security Vault

The Security Vault is Hydragent's lockbox, approval gate, and leak shield. Its job is simple: keep secrets out of model-visible text, keep risky actions behind explicit policy, and keep a durable record of what happened without turning the runtime into a surveillance mess.

This design borrows the useful parts of OpenClaw, Hermes, and OpenCode:

- OpenClaw: strong defaults, explicit security posture, and no convenience wrapper that hides important decisions.
- Hermes: dangerous-command approval, sandboxed execution with secrets stripped, and security scanning for injected instructions.
- OpenCode: durable context boundaries, permission-aware tool use, and audit-friendly separation between admitted state and active execution.

## 1. The Lockbox

Hydragent stores secrets in the vault, not in prompts, transcripts, or skill bodies.

The lockbox should support two unlock paths:

- Slot 0: a user-entered secret or PIN-derived unlock path for interactive use.
- Slot 1: a local admin or machine-bound file for trusted unattended startup.

Hydragent should keep the unlock story explicit:

- unlock happens locally
- the model never sees the raw secret
- the vault only exposes short-lived materialized values to the layer that actually needs them

### Model-Blind Injection

The model should only see placeholders, never the real value.

Example:

```text
Send request to provider using key {{VAULT:OPENROUTER_KEY}}
```

The real secret is injected only at the last safe boundary, after policy and before network dispatch or trusted child-process launch. That keeps the model from learning or echoing the actual value.

## 2. Secret Storage Rules

The vault should follow a few hard rules:

- secrets stay out of chat history
- secrets stay out of skill bodies
- secrets stay out of logs
- secrets stay out of error messages
- secrets stay out of tool output unless the tool explicitly returns a redacted placeholder

If Hydragent needs to mention a secret at all, it should mention the vault slot name, not the secret itself.

The right rule is not "be careful". The right rule is "the value never crosses into the wrong boundary".

## 3. Approval and Sandboxing

Hydragent should treat risky actions as approval-gated operations.

Examples of gated operations:

- shell commands that touch the host
- writes outside the active desk/workspace
- network actions that expose data externally
- tool calls that can read or emit sensitive files

The manager should make the decision, not the model.

When execution is allowed, it should happen in the narrowest safe sandbox available:

- strip API keys and secrets from child environments unless the child explicitly needs one secret
- pass only the minimum vault material needed for the current action
- keep the sandbox boundary clear in logs and audits
- do not let approval prompts become a covert channel for secrets

## 4. Dangerous Command Shield

Hydragent should scan proposed commands before they run.

The shield should catch obvious bad patterns like:

- shell injection attempts
- privilege escalation attempts
- command strings that try to smuggle secrets out
- prompt-injection text that tries to redirect policy or exfiltrate data

This is not about pretending all text is safe. It is about stopping obvious abuse before it reaches the shell or the network.

If the command is risky, Hydragent should stop and ask for approval instead of guessing.

## 5. Secret Leak Shield

Leak tracking should happen on the way out, not just on the way in.

Before Hydragent sends a reply to a chat room, terminal, or other outward channel, it should scan the payload for:

- vault placeholders that accidentally resolved to a real secret
- raw secret-looking values
- copied credential fragments
- paths or outputs that should stay local

If a leak is detected, Hydragent should redact the message, keep the incident in the audit trail, and warn the operator.

The shield should prefer redaction over silence when possible, because a redacted warning is better than a hidden failure.

## 6. The Chain of Logs

Every security-relevant action belongs in a durable audit chain.

The audit chain should record:

- approvals
- secret materialization events
- denied secret access
- sandbox launches
- dangerous command detections
- blocked leak attempts
- policy violations

Each entry should link to the previous one so tampering is obvious.

```text
[Day 1 Log] <- [Day 2 Log (hashed with Day 1)] <- [Day 3 Log (hashed with Day 2)]
```

The point is not crypto theater. The point is to make the log tamper-evident and easy to verify after the fact.

## 7. Audit and Recovery

If the audit chain is broken or a security invariant is violated, Hydragent should fail closed.

That means:

- stop the risky action
- preserve the evidence
- surface the incident to the operator
- avoid silently continuing with compromised assumptions

When possible, the system should keep the last clean state and continue only after the operator or policy layer resolves the issue.

## 8. Hydragent Identity Rules

The vault must still feel like Hydragent.

- use the lockbox, desk, draft paper, pages, and audit chain vocabulary
- keep the core orchestrator as the policy brain
- treat dreaming as cleanup and compaction, not as secret handling
- keep Graphify for durable relationships, not for exposing sensitive payloads
- keep security explicit and local, not hidden behind magic

The system can borrow OpenClaw-style strong defaults, Hermes-style approval and scanning, and OpenCode-style boundary discipline, but the product should still read as Hydragent.

## 9. Suggested Vault State Machine

```text
secret stored
	-> unlock requested
	-> policy checked
	-> approval granted or denied
	-> materialized only for the trusted boundary
	-> used
	-> scrubbed from transient memory
	-> recorded as audit metadata, not raw value

risky action
	-> scanned
	-> approved or blocked
	-> sandboxed if allowed
	-> audited
	-> leak-checked on exit
```

## 10. Practical Defaults

- Deny by default.
- Materialize secrets as late as possible.
- Strip secrets from child processes unless explicitly required.
- Never log raw secrets.
- Prefer redaction over disclosure.
- Keep operator-facing warnings short and actionable.
