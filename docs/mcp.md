# Use AARG from Claude (MCP server)

AARG can run as a [Model Context Protocol](https://modelcontextprotocol.io)
(MCP) server, so you can drive it by chatting with an MCP client like **Claude
Desktop** or **Claude Code**. Instead of switching to a terminal, you ask Claude
to read your dataset, size up a job posting, and tailor your résumé, and it calls
AARG for you. Everything runs through the same guards as the CLI, so nothing it
produces can claim experience your dataset doesn't support.

It's a hand-rolled stdio server (no SDK), the same way AARG's LLM clients are
written directly against the provider APIs.

## What you can ask for

Once it's connected (see below), you talk to Claude in plain English and it picks
the right tool. For example:

- *"Summarize what's in my AARG dataset."* Reads your recorded roles and skills,
  and which skills have backing evidence.
- *"Here's a job posting: [paste]. How well do I fit it?"* Parses the posting and
  shows the gap: what you match with evidence, what's weak, what's missing.
- *"Tailor my résumé to this posting, ATS version, with a cover letter."* Runs the
  full adversarial review loop and renders the PDFs.
- *"Re-tailor build 046."* Re-runs a past build against its stored posting, so you
  never have to paste the same job description twice.
- *"Show me the PDFs from that build."* The rendered PDFs are exposed as resources
  your client can list, open, and download.

If your client supports it, `tailor` also asks you the same questions the CLI does
mid-run (back a missing skill, add a real metric, sharpen a weak line), right in
the chat.

## Quick start (Claude Desktop)

1. Make sure AARG is installed and set up: `aarg --version`, and `aarg init` once
   if you haven't.
2. Get the binary's full path: `which aarg`. Claude Desktop doesn't use your
   shell's `PATH`, so it needs the absolute path.
3. Open Claude Desktop's config file:
   - macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`
   - Windows: `%APPDATA%\Claude\claude_desktop_config.json`
   - Linux: `~/.config/Claude/claude_desktop_config.json`
4. Add an `aarg` server, using your path from step 2:
   ```json
   {
     "mcpServers": {
       "aarg": { "command": "/home/you/.cargo/bin/aarg", "args": ["mcp"] }
     }
   }
   ```
5. Fully quit and reopen Claude Desktop. The AARG tools appear under the server,
   ready to use.

To sanity-check the server on its own, pipe a request to it:

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{}}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  | aarg mcp
```

You should see two JSON-RPC responses: the handshake and the tool list. Log lines
go to stderr and never mix into the protocol on stdout.

## Claude Code

```bash
claude mcp add aarg -- /absolute/path/to/aarg mcp
```

Or commit a project-scoped `.mcp.json` with the same `command`/`args`.

## Run it on another machine (SSH)

The server speaks over stdio, and an MCP client launches it as a local
subprocess, so to reach a server on another machine you make that subprocess
`ssh`: it carries the stdin/stdout stream to a remote `aarg mcp`. Your dataset,
credentials, and `typst` all stay on the server; only JSON-RPC crosses the wire,
encrypted by SSH. Install AARG on the server first, then point the client config
at `ssh`:

```json
{
  "mcpServers": {
    "aarg": {
      "command": "ssh",
      "args": ["-q", "-o", "ServerAliveInterval=30",
               "you@your-server", "/home/you/.cargo/bin/aarg", "mcp"]
    }
  }
}
```

Two things make this reliable:

- **SSH must never prompt.** Use key-based auth with a passphraseless key (or
  ssh-agent). A password or passphrase prompt would land in the JSON-RPC stream
  and break it.
- **Nothing may print to stdout around the server.** A login banner, MOTD, or a
  shell rc file that echoes corrupts the stream. `-q` silences SSH's own output;
  the durable fix is a dedicated key locked to the command in the server's
  `~/.ssh/authorized_keys`, which bypasses the login shell entirely:

  ```
  command="/home/you/.cargo/bin/aarg mcp",no-pty,no-port-forwarding,no-agent-forwarding ssh-ed25519 AAAA...your-key
  ```

  That key can do nothing but run `aarg mcp`: no shell, no MOTD, no other command.

The interactive copilots work over SSH too. The elicitation capability comes from
the client's handshake, not from a terminal.

## What the server needs

Wherever `aarg mcp` runs, it needs what the CLI needs on that machine:

- **Your AARG setup**: the dataset it tailors from and the API key it calls the
  model with. The server reads the same ones the CLI does on that machine.
- **`typst`**, which renders the PDFs. AARG finds it on `PATH`, and also looks
  next to its own binary and in `~/.cargo/bin`, `~/.local/bin`, and
  `/usr/local/bin`, so a `cargo install`ed typst is found with no setup. If yours
  lives elsewhere, point AARG at it with `AARG_TYPST=/path/to/typst` (easy to set
  in the SSH command) or `[render] typst = "/path/to/typst"` in config. `aarg
  config` shows the current setting.

### Auth on a headless server

The OS keychain needs a desktop or login session to unlock, which a server you
reach over SSH usually doesn't have. So on a headless box, give AARG its
credential a keychain-free way. Both options below are read at request time, so
no secret ends up in the config file:

- **An environment variable.** Export `ANTHROPIC_API_KEY`, or
  `ANTHROPIC_AUTH_TOKEN` for a plan token from `claude setup-token`, in the
  environment that launches the server. If those standard names are already
  taken by another tool (a coding agent reads `ANTHROPIC_API_KEY` to override
  its own login), set `api_key_env` / `auth_token_env` under `[anthropic]` to a
  private name and export that instead, leaving the standard vars free.
- **A credential command.** Tag the active key as CLI-delegated and point it at
  any command that prints the token on stdout: read a `0600` file, call your
  password manager, or hit a vault. AARG runs it on each request.

  ```toml
  [anthropic]
  active_key = "subscription"

  [anthropic.key_kinds]
  subscription = "cli"

  [anthropic.credential_commands]
  subscription = ["cat", "/home/you/.config/aarg/token"]
  ```

  With that, `aarg mcp` authenticates itself, with no wrapper script and no
  secret in the config. If you don't set a command, a CLI-delegated key defaults
  to the official `ant auth print-credentials --access-token`.

## The tools

| Tool | What it does | Changes anything? |
|------|--------------|-------------------|
| `dataset_summary` | Summarize your recorded dataset: contact, counts, which skills have evidence | no |
| `list_builds` | List past builds, newest first, with scores and coverage | no |
| `get_build` | Fetch one build: its tailored résumé, the reviewer's report, coverage, and PDF locations | no |
| `parse_job` | Parse a job posting into structured requirements | no |
| `analyze_gap` | Compare a posting against your experience: matched, weak, and unknown skills | no |
| `tailor` | Run the adversarial loop and render the PDFs, from a pasted posting or a past build's id | writes a new build |
| `ingest` | Rebuild your dataset from résumé text | overwrites the dataset |

## Good to know

- **It never fabricates.** `tailor` drives the same pipeline as the CLI: every
  line on the page traces to recorded, evidence-backed material, and the
  adversarial reviewer still vets the draft. The MCP layer adds no path for a
  claim to reach the page without that check.
- **The copilots can run in the chat.** During `tailor`, the questions the CLI
  asks mid-run surface as prompts in your client through MCP elicitation, when the
  client supports it. A client that doesn't just tailors as-is, like a piped CLI
  run. An elicitation answer is treated as your own input, the same as a typed CLI
  answer, and the never-fabricate guards hold regardless of who supplies it.
- **Where your PDFs are.** They render on the machine running the server. AARG
  exposes every build's PDFs as MCP resources, so a client like Claude Desktop can
  list and open them; the `tailor` and `get_build` results also report the file
  paths on the server. Those two results also carry an inline PNG preview of the
  résumé's first page, which a client that renders images shows in the chat, so you
  see the page itself without opening a file. A client that can't fetch a binary
  PDF blob inline still gets that preview; the PDFs themselves stay a link away.
- **Re-tailoring is free of re-pasting.** A build stores the posting it was made
  for, so `tailor` with a `build_id` reuses it. No re-paste, no re-parse.
- **`ingest` overwrites your dataset**, after copying the previous one to a
  timestamped backup (`dataset.json.mcp-bak-<timestamp>`) you can restore.
- **Credentials and privacy.** The server reads your Anthropic credential the same
  way the CLI does (the OS keychain, or `ANTHROPIC_API_KEY` / `ANTHROPIC_AUTH_TOKEN`),
  so keep secrets out of the config file. And `dataset_summary`, `analyze_gap`, and
  `get_build` surface your recorded career data to whatever client connects, so
  connect only clients you trust.

## Troubleshooting

- **The server shows "failed" in Claude Desktop.** Check the command path is
  absolute and correct, then read the MCP logs (macOS
  `~/Library/Logs/Claude/mcp-server-aarg.log`, Windows `%APPDATA%\Claude\logs\`,
  Linux `~/.config/Claude/logs/`). To tell a server problem from a connection
  problem, run the handshake snippet above against the binary directly.
- **"typst not found" when tailoring.** typst isn't on the server's `PATH`, common
  when the server is launched over SSH (which has a thinner `PATH` than your
  interactive shell). See [What the server needs](#what-the-server-needs): a
  `cargo install`ed typst is found automatically; otherwise set `AARG_TYPST`.
- **It asks me to paste the posting again.** You don't have to. Ask it to re-tailor
  a past build by id ("re-tailor build 046") and it reuses the stored posting.
- **It tailored without asking me anything.** Your client may not support
  elicitation, so the copilots are skipped and it tailors as-is. Run `aarg tailor`
  in a terminal for the full interactive flow.

## Limits and what's next

- The transport is stdio. It still reaches another machine through the SSH bridge
  above. A native HTTP transport (for a hosted, multi-user, or Claude Desktop
  URL-connector deployment) isn't built yet; that's the next step if it's needed.
- Requests are handled one at a time, in order. A long `tailor` call blocks the
  next request until it returns.
- Elicitation uses the spec's form mode (flat fields), so a copilot shows up as a
  simple confirm, choose-one, multi-select, or short-text prompt. A richer
  multi-step copilot becomes a short sequence of those.
