# freeq-raven crate

This crate is the Rust runtime for Raven. It joins Freeq chat rooms and AV
sessions, transcribes voice, replies in chat or speech, renders Raven's video
tile, and can hand heavier work to a configured local tool command.

The crate depends on Freeq runtime crates by git revision:

- `freeq-sdk`
- `freeq-av`
- `freeq-agent-kit`

Product behavior belongs here. Generic Freeq fixes should be pushed to
`chad/freeq`.

## Build

```bash
cargo build -p freeq-raven
cargo build --release -p freeq-raven
```

Optional local whisper support:

```bash
cargo build --release -p freeq-raven --features stt
```

Most Raven runs use Deepgram STT, so the default build does not require local
whisper.cpp.

## Run Directly

```bash
DEEPGRAM_API_KEY=... \
INCEPTION_API_KEY=... \
ELEVENLABS_API_KEY=... \
RAVEN_BSKY_APP_PASSWORD=... \
cargo run --release -p freeq-raven -- \
  --server wss://irc.freeq.at/irc \
  --channel '#alexandria' \
  --name raven \
  --nick Raven \
  --freeq-auth bluesky \
  --bsky-handle raven-alexandria.bsky.social \
  --bsky-did did:plc:5cyzpborqchuckjhxciekbll \
  --render-backend coin \
  --ghostly-character raven \
  --answer-provider inception \
  --answer-model mercury-2 \
  --inception-reasoning-effort instant
```

For normal local operation, use the root wrapper scripts:

```bash
make start
make logs
```

The root wrapper defaults Raven to `RAVEN_FREEQ_AUTH=bluesky`, so the Freeq
member identity is `did:plc:5cyzpborqchuckjhxciekbll` when
`RAVEN_BSKY_APP_PASSWORD` is present. Use `RAVEN_FREEQ_AUTH=did-key` for
throwaway local bot identities.

## Tests

```bash
cargo test -p freeq-raven --lib
cargo test -p freeq-raven identity --test identity_test
cargo test -p freeq-raven
```
