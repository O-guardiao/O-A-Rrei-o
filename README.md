# O Arreio

> **Power, harnessed.**
> *Arreio* (ah-RAY-oh) is Portuguese for **harness** — the gear that lets a rider direct an animal far stronger than themselves, without making it weaker.

**O Arreio is a deterministic agent runtime.** It executes AI agents under formal contracts: every action passes through a physical brake (process sandbox), carries an identity (zero-trust, per-invocation credentials) and leaves a receipt (hash-chained audit ledger). The model proposes; the harness disposes.

It is not another framework for building agents. It is the layer that makes the agents you already run — local LLMs, coding agents, automation bots — **safe to delegate to and possible to audit afterwards**.

## Why

Agents stopped failing because models were weak. They now fail because they are *capable without governance*: a loop that rewrites your filesystem, a tool call you never authorized, an action nobody can reconstruct afterwards. Every incident has the same root: **autonomy without brakes, identity or proof**.

O Arreio inverts the usual design. The AI is only the inference engine. State, security, memory and execution belong to the harness:

| | Typical agent frameworks | O Arreio |
|---|---|---|
| Control | The agent decides | The agent proposes; FSM + hypervisor approve |
| State | Implicit, in the context window | Explicit, in a persistent Blackboard |
| Security | Prompt guardrails | Sandbox, command blocklist, watchdog, DLP, vault |
| Audit | Logs, maybe | Hash-chained ledger + trajectory store, by construction |
| Runtime | Python, async | Synchronous Rust, no async runtime |

## What you get

- 🛑 **Hypervisor** — command blocklist (interceptor) + infinite-loop watchdog; agents physically cannot run what you did not allow.
- 🧾 **Auditable reasoning** — CoT / ToT / ReAct / PAL as harness-applied modes, every step a hash-chained ledger entry; PAL programs execute only inside the sandbox.
- 🪪 **Zero-trust agent identity** — signed credentials with capability scopes, verified on *every* tool invocation; expired credential denies everything.
- ⏪ **Checkpointed execution** — DAG scheduling with git checkpoints before each node; `arreio rollback` restores the last good state.
- 🔐 **Vault** — AES-256-GCM key storage (Argon2id master key), automatic rotation, DLP scanner that redacts secrets/PII before anything reaches a provider.
- 🔌 **9 LLM providers** — Ollama (local-first, raw TCP), OpenAI, Anthropic, Gemini, Azure, DeepSeek, Kimi, MiniMax, OpenRouter. Model-agnostic by design.
- 🔗 **Protocol hub** — MCP server (`arreio_*` tools), Google A2A adapter, and bridges for Claude Code, Cursor, Hermes and OpenClaw. Integrations talk protocols; the harness governs them.
- 🦀 **40 Rust crates, fully synchronous** — no tokio, no hidden state. 1,862 tests passing.

## Quickstart (early adopters)

Pre-built binaries are coming with v0.1. Today, build from source:

```bash
git clone https://github.com/O-guardiao/O-A-Rrei-o
cd O-A-Rrei-o
cargo build --release --bin arreio

# local-first: point it at your Ollama
./target/release/arreio init
./target/release/arreio run --model ollama:llama3 --spec examples/plugin-hello/plugin.yaml
./target/release/arreio status
```

Windows GNU-toolchain users: see `.cargo/config.toml.example`.

## Status — commissioned honestly

This project is developed with a commissioning methodology (PVC): **nothing hidden behind a green badge**. Current state:

- ✅ Runtime, hypervisor, ledger, credentials, vault, DAG/rollback, multi-provider: implemented and covered by 1,862 unit/E2E tests.
- ✅ **MCP stdio** (`initialize` + `tools/list` with real input schemas) and the **OpenAI bridge** (`/v1/models`) validated end-to-end by smoke tests (2026-06-12). Connect Claude Code with `arreio bridge claude`, Cursor with `arreio bridge cursor`, any OpenAI client with `arreio bridge hermes` — see [`BRIDGE.md`](BRIDGE.md).
- ⚠️ Bridges still need validation against the real GUI apps (Claude Desktop, Cursor) and long-lived-thread E2E tests; the OpenClaw bridge is connection-test only (task orchestration is roadmap).
- ❌ No macOS sandbox yet (Windows tested; Linux implemented, untested in CI).

If you hit friction, open an issue — early-adopter feedback is exactly what this phase is for.

## Licensing

- **Core: AGPL-3.0-only** (all `arreio-*` crates). The auditor must stay auditable — forever.
- **Using O Arreio locally** (CLI, self-hosted): no obligations. Run it, modify it for yourself, enjoy.
- **Integrating over MCP / A2A / REST**: your client, agent or product does **not** become AGPL. Copyleft reaches derivative works, not protocol peers.
- **Embedding the crates in a closed product, or offering a modified O Arreio as a network service without releasing your changes**: that requires a **commercial license** — open a GitHub issue titled `commercial license` and we'll talk.
- `examples/`: Apache-2.0. Documentation: CC BY-SA 4.0. Vendored `zmij`: MIT (upstream license preserved).

## Contributing

External contributions require a CLA (it keeps dual licensing possible — see [CONTRIBUTING.md](CONTRIBUTING.md)). Code comments follow the project convention of Brazilian Portuguese; issues and PRs in English or Portuguese are both welcome.

---

*O Arreio — the harness for the age of capable agents. O agente puxa. O Arreio segura.*
