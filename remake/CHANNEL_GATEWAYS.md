# Hydragent Channel Gateways

Hydragent channel gateways are the edge processes that turn platform-specific wire events into one canonical message contract for the core orchestrator, then turn core replies back into platform actions.

They are still part of Hydragent, not a separate product: the name stays on the gateway layer so the system identity remains visible from the edge all the way to the core.

The design here is intentionally close to the Hermes relay contract and the OpenCode session model:

- normalize at the edge
- key sessions from source discriminators, not from transport identity
- keep the core blind to platform quirks
- fail closed on auth, routing, and delivery errors
- treat streaming, retries, and reconnects as first-class behavior

## 1. Canonical contract

Every inbound message must be normalized into a `MessageEvent` with a frozen `SessionSource`.

```json
{
  "platform": "telegram",
  "chat_id": "-1001234567890",
  "chat_type": "group",
  "chat_name": "Hydragent Lab",
  "user_id": "58299123",
  "user_name": "Alice",
  "thread_id": null,
  "chat_topic": null,
  "user_id_alt": null,
  "chat_id_alt": null,
  "scope_id": null,
  "parent_chat_id": null,
  "message_id": "771",
  "is_bot": false
}
```

The gateway may carry extra adapter metadata internally, but only the normalized source and message body cross into the core.

Inbound payload shape:

```json
{
  "source": { "platform": "telegram", "chat_id": "-1001234567890", "chat_type": "group" },
  "message_id": "771",
  "content": "Analyze my homework.pdf",
  "attachments": [
    {
      "name": "homework.pdf",
      "mime_type": "application/pdf",
      "local_path": "remake/scratch/downloads/homework.pdf"
    }
  ],
  "timestamp": 1700000000
}
```

## 2. Session keying

The gateway must key conversations from the normalized source, not from the socket, process, or bot token that happened to deliver the event.

Rules:

1. `chat_id` is the primary conversation discriminator.
2. `thread_id` further splits threaded conversations.
3. `scope_id` isolates shared-platform scopes such as Discord guilds and Slack workspaces.
4. `user_id` or `user_id_alt` is used when the platform requires per-user isolation.
5. If a discriminator is missing, the gateway must fall back in a deterministic order and document the collapse.

This prevents the worst failure mode: two different tenant spaces colliding into one session because the adapter guessed wrong about scope.

## 3. Transport and handshake

The core orchestrator and the gateways run as separate processes and talk over a local loopback channel using newline-delimited JSON-RPC.

Required control methods:

1. `gateway.register` - announce `platform`, adapter identity, and capability flags.
2. `gateway.heartbeat` - prove liveness and update the core's routing table.
3. `gateway.message` - deliver normalized inbound events.
4. `gateway.reply` - deliver outbound text, edits, threads, and action results.
5. `gateway.close` - signal clean shutdown or drain.

Handshake rules:

1. A gateway must not send traffic before registration succeeds.
2. Registration is fail-closed: if a platform-specific profile cannot authenticate, the gateway stays offline.
3. Heartbeats are periodic and local; loss of heartbeat marks the gateway unavailable and prevents new deliveries.

## 4. Core routing contract

The core owns intent, policy, scheduling, and model selection. The gateway owns platform normalization and platform egress.

The gateway must never:

- choose the model
- rewrite the conversation history
- invent session identity
- retry a denied action as a different action
- downgrade an auth failure into a silent success

The gateway must always:

- preserve the originating source fields
- attach platform timestamps and message IDs when available
- preserve thread and scope discriminators
- forward attachments through the sandbox download path
- surface delivery failures instead of pretending the message was sent

## 5. Streamed responses

Long responses are streamed back through the gateway as a sequence of bounded updates rather than a single final blob.

Supported behaviors:

- token or chunk streaming for platforms that support edits or draft updates
- one-message-per-segment fallback for platforms that do not support edits
- tool-progress suppression for channels that cannot present progress cleanly
- explicit long-running warnings when a tool or model turn stalls

The gateway owns the platform-specific rendering policy, but the core decides what semantic event happened.

## 6. Platform-specific adapters

Each adapter is thin and only translates platform rules into the canonical envelope.

### Telegram

- map chat, thread, and forum topic identifiers into `SessionSource`
- verify webhook or polling authenticity before accepting messages
- normalize inline buttons and callback queries into intent-bearing actions
- keep Markdown escaping strict on outbound text

### Discord

- respond only when the bot is mentioned or explicitly addressed
- strip mention markup before forwarding content to core
- map guild scope into `scope_id`
- map thread channels into `thread_id`
- keep the raw interaction or embed payload out of the model-facing contract unless it is normalized first

### Slack

- map `thread_ts` into `thread_id`
- map workspace into `scope_id`
- preserve top-level channel versus thread behavior as separate conversation lanes

### CLI and local UI

- use the same canonical event envelope as remote platforms
- skip platform auth, but still enforce local policy and deduplication
- keep local command handling inside the gateway, not in the core

## 7. Safety and abuse control

Every gateway should apply the same defensive filters before a message reaches the core.

Required controls:

- deduplicate repeated platform message IDs
- rate limit abusive senders
- block unauthorized users or rooms
- reject unsafe attachment types
- store downloads only in a restricted scratch area
- preserve webhook signatures or equivalent edge proofs before forwarding

If a platform cannot be trusted to prove message origin, the gateway must reject the event rather than pass a guessed payload inward.

## 8. Delivery failure rules

Gateway behavior must be boring under failure.

Rules:

1. If the outbound channel is unavailable, queue only if the platform contract explicitly supports replay.
2. If replay is not supported, fail the delivery and keep the core transcript accurate.
3. If the platform returns a permanent rejection, do not retry it as a different kind of message.
4. If a gateway crashes, the supervisor should restart it with backoff instead of cascading the failure into the core.

## 9. What this means for remake

For Hydragent Remake, the clean split is:

- gateways own platform adapters, auth, attachments, dedup, and rendering
- the core owns sessions, planning, tools, policy, and memory
- the boundary between them is a typed normalized message contract, not ad hoc chat strings

That gives us one route for Telegram, Discord, Slack, CLI, and webhook sources without teaching the core any platform-specific behavior.
