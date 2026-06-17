# AGENTS.md

`freeq-raven` is Raven's product repo. It owns Raven-specific Freeq behavior,
deployment wrappers, and tool handoff policy.

## Alexandria Hosted Instances

Use `ops/alexandria-instance-runbook.md` when configuring Raven for a hosted
Alexandria product instance.

Important boundaries:

- Raven is the only Freeq-facing loop for the instance.
- Do not add `freeqcc` to this deployment path.
- Heavy work goes through `RAVEN_TOOL_COMMAND`.
- The default heavy-work runner is `bin/raven-claude-runner`.
- Claude Code must return stdout to Raven; it must not join Freeq or post to
  the room directly.
- `RAVEN_TOOL_WORKDIR` must point at the target product repo, not at
  Alexandria's private maintainer repo unless the room explicitly asks to work
  on Alexandria itself.
- Project-local Alexandria/Fabro tools should be used from the target repo.

The legacy `bin/raven-tool-runner` Codex handoff remains available only as an
explicit fallback.
