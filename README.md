# freeq-raven

`freeq-raven` is Raven's product repo: a heavily customized Freeq chat and AV
agent that joins real Freeq rooms, participates in calls, keeps one room
session context, and can hand heavier work to local tools.

Freeq itself remains the protocol/runtime dependency. Raven-specific behavior
belongs here, not in Freeq examples.

## Repository Boundaries

- `chad/freeq`: upstream Freeq runtime, SDK, AV, bot, and example code. Generic
  fixes should go there directly.
- `sociotechnica-org/freeq-raven`: Raven product behavior, prompts, model
  routing, local launch/deployment, and tool handoff policy.
- target product repos: the work Raven inspects or edits when she runs Codex,
  Alexandria, Fabro, tests, or deployment tools.

This repo depends on Freeq crates from `chad/freeq` by git revision. It does not
vendor or patch `freeq-eliza`.

## Current Shape

```text
freeq-raven/
  Cargo.toml
  crates/
    freeq-raven/          # Rust runtime: chat, AV, STT, TTS, video, routing
  bin/
    freeq-raven           # loads .env and execs target/release/freeq-raven
    freeq-raven-start     # background/tmux launcher
    raven-tool-runner     # Codex handoff command
  ops/
    systemd/              # Linux service template
```

The old `.deps/freeq` bootstrap path is retired. `make bootstrap` now builds
this standalone Rust workspace.

## Architecture

Raven should be one agent loop, not one chat bot plus one AV bot plus a separate
tool brain. Chat, voice, tool results, and agent replies all flow through the
same runtime process and same per-channel context.

```text
Freeq IRC + AV
  -> chat adapter
  -> AV adapter: VAD, STT, TTS, video tile
  -> shared per-channel session context
  -> Raven turn router/planner
  -> chat reply, voice reply, typing/presence, or tool handoff
  -> tool result appended back into the same context
```

The current implementation keeps the shared session context in memory inside
the Rust runtime. The next architecture milestone is a durable `raven-session`
crate backed by SQLite so chat, AV, and tool events can be replayed and audited.

## Model Loop

The live loop defaults to Inception `mercury-2` because it is fast enough for
room conversation. The runtime gives the model a route choice:

- `chat`: answer immediately in the room.
- `tool_now`: run the tool command and post the result when it exits.
- `background`: acknowledge and let the tool command work without blocking the
  room.

The runtime still owns hard safety gates: ignore self messages, suppress obvious
bot loops, dedupe smoke-test style prompts, enforce tool timeouts, and emit
typing indicators while tool work is running.

Mercury is not assumed to be the final planner. The code should evolve toward a
planner abstraction so simple chat can use a fast model while tool/background
decisions can be promoted to a stronger model.

## Freeq Integration

Raven uses:

- `freeq-sdk` for IRC connection, SASL identity, agent registration, chat,
  typing indicators, and AV signaling.
- `freeq-av` for MoQ media session publish/subscribe, participant audio taps,
  speaker output, and video handles.
- `freeq-agent-kit` for VAD, addressed-name detection, hallucination cleanup,
  and speech/link splitting.

Raven joins `irc.freeq.at` and `#alexandria` by default.

## Prerequisites

- Rust toolchain with `cargo`
- Git
- optional: `tmux` for durable local background runs
- optional: authenticated `codex` CLI for heavy-work handoff
- API keys for the live AV loop:
  - `INCEPTION_API_KEY`
  - `DEEPGRAM_API_KEY`
  - `ELEVENLABS_API_KEY`

Install Rust on a fresh machine:

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

For local iteration, `.env` usually starts with:

```bash
FREEQ_SERVER=wss://irc.freeq.at/irc
FREEQ_CHANNEL=#alexandria
RAVEN_FREEQ_NICK=Raven
RAVEN_IDENTITY_NAME=raven
RAVEN_TOOL_WORKDIR=/absolute/path/to/target-product-repo
```

Never commit `.env`. It contains live provider keys.

## Local Run Loop

Build:

```bash
make bootstrap
```

Start Raven in the background:

```bash
make start
```

Inspect:

```bash
make status
make logs
```

Restart after a code change:

```bash
make restart
```

Stop:

```bash
make stop
```

The local wrapper writes:

- `.runtime/freeq-raven.pid`
- `.runtime/freeq-raven.log`

## Smoke Test

1. Run `make start`.
2. Open `https://irc.freeq.at/#` and join `#alexandria`.
3. Send:

   ```text
   Raven, reply with exactly: raven repo smoke ok
   ```

4. Start or join the voice call in `#alexandria`.
5. Confirm the call participant list includes Raven.
6. Say:

   ```text
   Raven, can you hear me?
   ```

7. Confirm Raven replies by voice and her video tile renders.

## Tool Handoff

`RAVEN_TOOL_COMMAND` receives JSON on stdin. The default command is:

```bash
bin/raven-tool-runner
```

The runner executes Codex in `RAVEN_TOOL_WORKDIR`, which should be the target
product repository. It should not operate in Alexandria's private maintainer
repo unless the room explicitly asks Raven to inspect Alexandria itself.

Alexandria/Fabro integration should happen through project-local/public
Alexandria tooling installed in the target product repo. Private maintainer
skills are intentionally outside this repo's runtime contract.

## Systemd Deployment

For a Linux box, copy `ops/systemd/freeq-raven.service` into the user service
directory and edit paths if the repo is not cloned at `/opt/freeq-raven`:

```bash
mkdir -p ~/.config/systemd/user
cp ops/systemd/freeq-raven.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now freeq-raven
journalctl --user -u freeq-raven -f
```

During rapid iteration, local `make restart` is usually faster. The systemd
unit is for an always-on staging agent.

## Development Checks

```bash
make check
make test
cargo test -p freeq-raven
```

The full e2e tests spin up in-process Freeq servers and can take longer than
the unit tests. The default `make check` path runs fast compile and identity
coverage first.

## Roadmap

- Add durable SQLite `raven-session` event log.
- Split model routing behind a planner trait.
- Promote tool/background decisions to a stronger model when Mercury is unsure.
- Emit richer Freeq-native task events for background work.
- Add a watcher/supervisor process that can wake/restart Raven but never answer
  room messages itself.
