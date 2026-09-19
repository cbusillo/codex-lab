# Account proxy (spike)

Spike for [#951](https://github.com/cbusillo/codex-lab/issues/951), part of the thin-fork
decision in [#926](https://github.com/cbusillo/codex-lab/issues/926). Rate-limit-aware switching
between ChatGPT accounts as a loopback proxy, so it works with a stock Codex engine.

```sh
uv run --script account_proxy.py            # reads ~/.codex-lab/auth_accounts.json, read-only
codex -c 'openai_base_url="http://127.0.0.1:8787/backend-api/codex"' …
```

The URL must end in `/backend-api/codex` or Codex disables its ChatGPT-login routes. The proxy
replaces `Authorization` and `ChatGPT-Account-Id`, and on HTTP 429 with error type
`usage_limit_reached` it blocks that account and replays the same request on the next one.
The limit arrives as a status before any streamed bytes, so the client never sees it.

Not for real use as it stands:

- **It never refreshes tokens.** A refresh rotates the refresh token, so whichever component
  refreshes must own the store. Here Codex Lab owns it and the proxy only reads.
- Any local process can use the port. A real version needs a shared secret.
- WebSocket upgrades are refused with 426; Codex uses HTTPS.
- Bodies are passed through untouched (Codex compresses requests with zstd).
