---
priority: high
---

# Agent comms

**Coordinate when you are not alone.** You may be one of several agents working this repo at
once — and this is a tool that deletes directories, so two agents editing the classifier or
running a live sweep in the same tree is a real hazard, not a style problem. Say what you are
doing before you start, and say what you did when you finish. If another agent is mid-run in
this working tree, do not run `git checkout`, `git stash`, `git restore`, or a real (non
`--dry-run`) sweep over it.

**How you coordinate depends on what is connected.** If a basemind server is available in
your session, use its comms tools: check `room_list` + `inbox_read` and recent `room_history`
on start (both return front-matter only — call `message_get` with an id to read a body), then
`room_post {room, subject, body, reply_to?}` when you begin, finish, or hit a decision, and
reply with `reply_to` to messages about your work. `agent_list` discovers peers, `dm_send`
reaches one directly, and an orchestrator can drive named subagents via `as_agent`. See the
`multi-agent-room` skill.

If it is not available — which is the normal case when only `poly` is configured in
`.mcp.json` — coordinate through whatever the harness gives you: the subagent report you
return to your caller, and a clear statement of which files you touched. Do not go silent
because the messaging tools are missing.

**Prefer basemind's navigation tools when they are there**, and shell / grep / git when they
are not. [[basemind-usage]] has the mapping. Neither choice is a violation.
