# freeq-raven

`freeq-raven` runs Raven as a Freeq chat and AV agent. It is intentionally
separate from `chad/freeq`: this repository owns the Raven product behavior,
startup surface, secrets layout, and heavy-work handoff policy. Freeq remains
the transport/runtime dependency.

## Architecture

Raven has two loops:

1. **Hot conversation loop**
   - joins Freeq chat and AV rooms
   - subscribes to Freeq AV over MoQ
   - transcribes human audio with Deepgram
   - asks Inception `mercury-2` whether each addressed turn should stay
     in chat, call a subagent now, or background subagent work
   - answers normal chat and voice turns with Inception `mercury-2`
   - speaks via ElevenLabs TTS
   - records chat, voice, agent replies, and tool results into one shared
     per-channel session context

2. **Heavy-work loop**
   - only runs when Mercury routes the turn to `tool_now` or `background`
   - receives a JSON payload from the hot loop
   - runs Codex in the configured target product repository
   - uses project-local Alexandria/Fabro tools when present
   - posts the concise result back through Raven

Routing is intentionally model-owned. Mercury is Raven's fast room brain; it
decides when to subagent work. The Rust runtime only enforces the execution
boundary, passes the selected task to the local runner, and keeps exact
echo/marker prompts on the hot chat path for smoke tests.

## Routing Policy

Mercury can choose:

- `chat` — answer immediately in the room. Use for discussion, quick advice,
  exact repeat prompts, and questions about the current conversation.
- `tool_now` — run Codex in the target repo and return the result as soon as
  it finishes. Use for small inspections or checks where the room is waiting.
- `background` — acknowledge, let the room continue, and post back later. Use
  for implementation work, Alexandria/Fabro plays, broad audits, test suites,
  deploys, and other multi-step work.

While `tool_now` or `background` work is running, Raven emits Freeq typing
indicators every five seconds. Freeq auto-clears stale typing indicators after
10 seconds, and Raven sends `typing_stop` when the subagent exits.

Raven does not use private Alexandria maintainer skills. If a target product
uses Alexandria, install the public/project-local Alexandria skills into that
target repo and point `RAVEN_TOOL_WORKDIR` at it.

## Why This Repo Patches Freeq

The current Raven runtime depends on a small set of `freeq-eliza` changes that
are not upstream in `chad/freeq` yet. To keep this repo operational without
requiring Chad to accept Raven-specific behavior, `bin/freeq-raven-bootstrap`
clones Freeq into `.deps/freeq`, checks out a known base commit, applies
`patches/freeq-raven-eliza.patch`, and builds `freeq-eliza`.

When the generic Freeq pieces land upstream, this repo can drop the patch and
depend on an upstream Freeq commit directly.

## Prerequisites

- macOS or Linux with Bash
- Rust toolchain with `cargo`
- Git
- `codex` CLI authenticated if you want heavy-work handoff
- API keys for Inception, Deepgram, and ElevenLabs

On a fresh machine, install Rust with:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

## Setup

```bash
git clone git@github.com:sociotechnica-org/freeq-raven.git
cd freeq-raven
cp .env.example .env
$EDITOR .env
make bootstrap
```

For local development on Jess's machine, `.env` should usually include:

```bash
FREEQ_CHANNEL=#alexandria
RAVEN_TOOL_WORKDIR=/Users/jessmartin/Documents/code/wedo
```

Store API keys in `.env`; it is ignored by Git.

## Run

```bash
make start
make status
make logs
```

Stop or restart:

```bash
make stop
make restart
```

The service writes:

- `.runtime/freeq-raven.pid`
- `.runtime/freeq-raven.log`

## E2E Smoke Test

1. Start Raven:

   ```bash
   make start
   ```

2. Open Freeq in Chrome at `https://irc.freeq.at/#` and join `#alexandria`.
3. Send a chat message:

   ```text
   Raven, reply with exactly: raven repo smoke ok
   ```

4. Start or join the voice call in `#alexandria`.
5. Confirm the call participant count includes `raven-*`.
6. Say:

   ```text
   Raven, can you hear me?
   ```

7. Confirm Raven answers by voice.

## Commands

- `make bootstrap` clones, patches, and builds Freeq.
- `make start` runs Raven as a background service. On machines with `tmux`,
  it uses a `freeq-raven` tmux session so the service survives terminal exits.
- `make stop` stops the background service.
- `make status` shows process and Freeq session state.
- `make logs` tails the service log.
- `make check` verifies the patch applies and the binary builds.

## Security

Never commit `.env`, `.runtime`, `.deps`, logs, pid files, or Freeq identity
files from `~/.freeq`. API keys used during early experiments should be rotated
before making this repository public.
