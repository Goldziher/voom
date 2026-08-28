---
priority: medium
---

# basemind, when it is connected

[basemind](https://github.com/Goldziher/basemind) is an indexed context layer served over MCP.
When it is connected in your session it is the better way to navigate this repo: its tools
return paths, line numbers, and signatures — a fraction of the tokens of reading source — and
they share one index across the session.

**It is not guaranteed to be there.** This repo's `.mcp.json` is generated from
`.ai-rulez/config.toml`, and whether a basemind server is actually running depends on the
harness and the machine. So: **check whether the tools are available, use them if they are,
and use shell / grep / git if they are not.** Neither is a violation. Do not spend turns
hunting for a server that is not connected, and do not refuse to answer a question because
the preferred tool is absent.

Below is the mapping, for when the tools *are* available. Each line is "prefer the left over
the right", not "the right is forbidden".

## 1. Code search — over grep / ripgrep / reading whole files

- `search_symbols` — "where is X defined?"
- `outline` — the shape of a file (symbols, signatures, imports); add `l2: true` for calls +
  docs. Outline a file before you open it, then read only the span you need.
- `find_references` — every use site of a name.
- `find_callers` — callers of a specific definition.
- `workspace_grep` — full-text search across the repo.

After edits, `rescan` rather than reconnecting.

## 2. Git history — over naked `git log` / `git blame` / `git diff`

- `recent_changes` — what changed recently.
- `blame_file` / `blame_symbol` — who last touched a file or symbol.
- `diff_file` / `diff_outline` — diffs at file or symbol granularity.
- `commits_touching` — history for a path/symbol.

## 3. Crawling & document intelligence — when researching crate APIs / docs

- `web_scrape` / `web_crawl` / `web_map` — scrape a page, crawl a site, or fetch a sitemap when
  researching an upstream crate's API or documentation.
- `search_documents` and the documents pipeline (RAG, keyword + entity/NER, summary) — extract
  and search over docs/PDFs/specs in the repo instead of opening them by hand.

## 4. Spawning subagents via basemind shells

- `shell_spawn` / `shell_send` / `shell_broadcast` / `shell_list` / `shell_capture` /
  `shell_kill` — spawn and drive subagents, in addition to `as_agent` / `dm_send`. The
  harness's own subagent mechanism does the same job when basemind is absent.

## 5. Agent communication — coordinate with peers

- `agent_list` — discover other agents on the repo.
- `room_list` / `room_history` / `inbox_read` / `message_get` — read what's been said
  (`room_history` / `inbox_read` return front-matter only; call `message_get` with an id for a
  body).
- `room_post` / `dm_send` — post status when you begin, finish, or hit a decision; DM a
  specific peer.

See [[agent-comms]] for the coordination side of this.
