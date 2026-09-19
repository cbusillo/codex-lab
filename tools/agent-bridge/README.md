# Agent bridge (spike)

Spike for [#949](https://github.com/cbusillo/codex-lab/issues/949), part of the thin-fork
decision in [#926](https://github.com/cbusillo/codex-lab/issues/926). Third-party coding
agents as MCP tools, so delegation works from any MCP host without engine support.

`agent_bridge.py` is a single-file MCP server (stdio). Tools: `spawn`, `wait`, `cancel`,
`list`. Providers: `claude` and `antigravity` (`agy`), launched with the same arguments Codex
Lab uses natively. It needs `uv` and the provider CLIs on `PATH`, each already signed in.

## Use it

Stock Codex, one-off (no config file change):

```sh
codex exec -c 'mcp_servers.agent_bridge={command="uv",args=["run","--script","/abs/path/agent_bridge.py"],tool_timeout_sec=400,default_tools_approval_mode="approve"}' "…"
```

`default_tools_approval_mode="approve"` is required for non-interactive runs; without it
Codex refuses with "MCP tool call requires approval, but approval policy is never".

Claude Code:

```sh
claude -p "…" --mcp-config '{"mcpServers":{"agent_bridge":{"command":"uv","args":["run","--script","/abs/path/agent_bridge.py"]}}}' \
  --allowedTools "mcp__agent_bridge__spawn,mcp__agent_bridge__wait,mcp__agent_bridge__cancel,mcp__agent_bridge__list"
```

## Check it

`uv run --script smoke_client.py` drives the server as a host would: a real PONG task per
provider, and a cancel check against a stand-in provider that holds a grandchild process open.
Real agents background long commands and return, so they cannot exercise cancel.

## Limits

Output is capped at 64 KiB, tasks at 64 KiB, eight concurrent agents, 30-minute default
timeout. Each agent leads its own process session; cancel, timeout and server exit signal the
whole group. State is in memory and is lost when the host closes the server.
