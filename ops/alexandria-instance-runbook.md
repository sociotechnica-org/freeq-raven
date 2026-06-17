# Raven Alexandria Instance Runbook

Use this runbook when Raven is the Freeq-facing agent for a hosted Alexandria
product instance.

The Alexandria-side operator runbook lives in the Alexandria internal repo at
`docs/alexandria/ops/product-hosting-runbook.md`. This repo owns the Raven
runtime, environment variables, and heavy-work runner.

## Boundary

- Raven is the only process that joins Freeq and posts to the room.
- Do not deploy `freeqcc` for this path.
- Claude Code is a backend reached through `RAVEN_TOOL_COMMAND`.
- Claude Code must return stdout to Raven; it must not join Freeq or hold Freeq
  credentials.
- `RAVEN_TOOL_WORKDIR` is the target product repo for the Alexandria instance.
- Project-local Alexandria/Fabro tools should be used from that target repo.
- The legacy Codex runner is available only by explicitly setting
  `RAVEN_TOOL_COMMAND=bin/raven-tool-runner`.

## Required Variables

Set these for every hosted instance:

```bash
FREEQ_SERVER=wss://irc.freeq.at/irc
FREEQ_CHANNEL=#alexandria
RAVEN_FREEQ_NICK=Raven
RAVEN_IDENTITY_NAME=raven

RAVEN_TOOL_WORKDIR=/data/projects/freeq-raven
RAVEN_TOOL_COMMAND=/app/bin/raven-claude-runner
```

For local trusted-machine mode, leave `RAVEN_CLAUDE_ENDPOINT` unset and make
sure `claude` is installed, authenticated, and on `PATH`.

For Railway mode, keep Claude Code on a trusted Tailnet machine and set:

```bash
RAVEN_CLAUDE_ENDPOINT=http://claude-code-1:8765
RAVEN_CLAUDE_ENDPOINT_TOKEN=...
```

Optional controls:

```bash
RAVEN_CLAUDE_MODEL=sonnet
RAVEN_CLAUDE_PERMISSION_MODE=bypassPermissions
```

`RAVEN_TOOL_MODEL` is still honored as a compatibility alias for the Claude
model, but new deployments should prefer `RAVEN_CLAUDE_MODEL`.

## Local Trusted-Machine Mode

Use this for the fastest first instance or for debugging the runner without a
remote worker.

1. Install and authenticate Claude Code on the trusted machine.
2. Clone the target product repo.
3. Install or initialize Alexandria Next in the target repo.
4. Clone `freeq-raven`.
5. Copy `.env.example` to `.env`.
6. Set `RAVEN_TOOL_WORKDIR` to the target product repo.
7. Set `RAVEN_TOOL_COMMAND` to the absolute path of
   `bin/raven-claude-runner`.
8. Leave `RAVEN_CLAUDE_ENDPOINT` unset.
9. Run:

   ```bash
   make bootstrap
   make start
   make logs
   ```

10. In Freeq, run the chat and heavy-work smoke tests.

## Railway Mode

Use this when Raven and the Alexandria viewer/runtime run on Railway while
Claude Code stays on a trusted Tailnet machine.

The Railway wrapper uses these Alexandria variables:

```bash
ALEXANDRIA_INSTANCE_ID=freeq-raven
ALEXANDRIA_PROJECT_REPO=https://github.com/sociotechnica-org/freeq-raven.git
ALEXANDRIA_PROJECT_BRANCH=main
ALEXANDRIA_DATA_DIR=/data
ALEXANDRIA_NEXT_WORKSPACE=/data/workspaces/freeq-raven
```

The wrapper derives:

```bash
RAVEN_TOOL_WORKDIR=/data/projects/$ALEXANDRIA_INSTANCE_ID
RAVEN_TOOL_COMMAND=/app/bin/raven-claude-runner
```

Set the Freeq and Claude worker variables:

```bash
FREEQ_CHANNEL=#alexandria
RAVEN_FREEQ_NICK=Raven
RAVEN_IDENTITY_NAME=raven
RAVEN_CLAUDE_ENDPOINT=http://claude-code-1:8765
RAVEN_CLAUDE_ENDPOINT_TOKEN=...
```

The host image or supervisor must start Tailscale before Raven tries to reach
`RAVEN_CLAUDE_ENDPOINT`. The Tailnet route should allow the Railway node to
reach only the Claude worker and Alexandria ACP endpoints it needs.

## Add A New Project/Channel Instance

1. Choose a new `ALEXANDRIA_INSTANCE_ID`.
2. Choose the target project repo and branch.
3. Choose the Freeq channel, Raven nick, and Raven identity name.
4. Create a fresh Railway service and persistent volume.
5. Set `ALEXANDRIA_PROJECT_REPO` to the target repo.
6. Set `FREEQ_CHANNEL`, `RAVEN_FREEQ_NICK`, and `RAVEN_IDENTITY_NAME` for that
   room.
7. Create a new trusted-machine worker directory and endpoint for the instance.
8. Set `RAVEN_CLAUDE_ENDPOINT` to that Tailnet-only endpoint.
9. Deploy one Railway replica.
10. Run the smoke tests.

Do not reuse a volume, Raven identity, Tailnet hostname, or worker lock between
project/channel instances.

## Smoke Tests

Start Raven:

```bash
make start
make logs
```

In Freeq:

```text
Raven, reply with exactly: raven repo smoke ok
```

Then verify heavy work:

```text
Raven, use your tool to report the current repo branch and whether git is clean.
```

The passing result is:

- Raven posts the result, not Claude Code;
- the command runs in `RAVEN_TOOL_WORKDIR`;
- the branch matches the configured project branch;
- dirty Git state is reported clearly;
- Claude Code never joins Freeq or sends its own room message.

## Recovery

Claude runner missing:

1. Check `RAVEN_TOOL_COMMAND`.
2. In local mode, run `which claude` on the trusted machine.
3. In Railway mode, check `RAVEN_CLAUDE_ENDPOINT` and Tailnet routing.
4. Run the heavy-work smoke test again.

Dirty target repo:

1. Stop new tool requests.
2. Inspect `git -C "$RAVEN_TOOL_WORKDIR" status --short --branch`.
3. Commit/push intentional changes.
4. Restore only generated or explicitly approved files.
5. Restart Raven.

Wrong repo:

1. Stop Raven.
2. Correct `RAVEN_TOOL_WORKDIR`.
3. Restart Raven.
4. Ask Raven to report `pwd` and `git remote -v`.
