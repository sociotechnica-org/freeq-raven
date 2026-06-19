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
cargo run --release -p freeq-raven -- \
  --server wss://irc.freeq.at/irc \
  --channel '#alexandria' \
  --name raven \
  --nick Raven \
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

## Tests

```bash
cargo test -p freeq-raven --lib
cargo test -p freeq-raven identity --test identity_test
cargo test -p freeq-raven
```
