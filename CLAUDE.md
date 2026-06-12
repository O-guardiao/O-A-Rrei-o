# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Environment requirements

Rust toolchain: `stable-x86_64-pc-windows-gnu` (installed at `%USERPROFILE%\.cargo\bin`).
C toolchain: MSYS2 ucrt64 at `C:\msys64\ucrt64\bin` — provides `gcc`, `dlltool`, `ar` etc.

Both must be in PATH before any `cargo` command:
```powershell
$env:PATH = "$env:USERPROFILE\.cargo\bin;C:\msys64\ucrt64\bin;$env:PATH"
```

Build artifacts go to `C:\dev\omni-target` (set in `.cargo/config.toml`) to stay outside OneDrive, where Windows AppControl blocks compiled binaries.

## Build and test

```powershell
# check types across all crates (fast)
cargo check --workspace

# run all tests
cargo test --workspace

# run tests for a single crate (use when AppControl blocks a newly-compiled binary on first run — retry works)
cargo test -p arreio-kernel
cargo test -p arreio-fsm
cargo test -p arreio-actors
cargo test -p arreio-hypervisor
cargo test -p arreio-dag
cargo test -p arreio-ast
cargo test -p arreio-cli

# build CLI binary
cargo build --bin arreio

# smoke test the CLI
cargo run --bin arreio -- init
cargo run --bin arreio -- status
cargo run --bin arreio -- skills
```

## Workspace structure

The workspace has grown from the original seven crates to **40 crates** organized around the **SYMBION architecture** — ten cognitive subsystems based on historically validated techniques (SOAR, OODA-C, Design by Contract, Blackboard, Recovery Blocks, Refinement Calculus, Autopoiesis, etc.).

### Core crates (original)

| Crate | Role |
|---|---|
| `arreio-kernel` | Blackboard (HEARSAY-II shared state) + Tuple Space, JSON-persisted + vector store com backend plugável (`VectorBackend`: linear default, HNSW opt-in — PVC-Q4.2/ADR-0014) |
| `arreio-fsm` | FSM with 9 states; state stored in Blackboard |
| `arreio-actors` | Architect / Developer / Inspector / **Refiner** actors + raw-TCP Ollama client + enriched ActorContext (RetryContext, trajectory_window, architect_rationale) |
| `arreio-hypervisor` | Process sandbox: command blocklist (interceptor) + loop-detection watchdog |
| `arreio-dag` | DAG engine: topological ready-node scheduling + git-based checkpoints |
| `arreio-ast` | Rust symbol-map extractor (`syn`) + regex fallback for other languages |
| `arreio-cli` | `clap`-based entry point: `init`, `run`, `status`, `rollback`, `skills` + PVC-Q4.1: `commission`, `credential`, `reason`, `score` e flags `--agent-credential`/`--reasoning-mode`/`--prioritized` (wiring em `src/cmd_wiring.rs`, ADR-0013) |

### Infrastructure crates

| Crate | Role |
|---|---|
| `arreio-provider` | Multi-provider LLM client (Ollama TCP, OpenAI/Anthropic HTTP/1.0 raw) |
| `arreio-memory` | Memory engine: typed envelopes, GraphStore, hybrid recall, lifecycle |
| `arreio-skills` | Skill Store + auto-learning + trigger matching |
| `arreio-gateway` | Synchronous HTTP server + REST API + embedded SPA dashboard |
| `arreio-lsp` | LSP JSON-RPC 2.0 client over stdio |
| `arreio-security` | Security policies, credential scanning, auth guardrails |
| `arreio-scheduler` | Task scheduler + priority queue + resource allocation |
| `arreio-telemetry` | Metrics, tracing and Blackboard-based observability |
| `arreio-mcp` | Model Context Protocol (MCP) adapter |
| `arreio-tools` | Tool registry + dynamic invocation + Skill CRUD tools + MCP bridge + policy engine + native handlers (file, exec, web, memory, checkpoint, media) |
| `arreio-agents` | Agent orchestration layer (multi-agent coordination) |
| `arreio-web` | Web interface components |
| `arreio-tui` | Terminal UI (crossterm-based) |
| `arreio-media` | Media processing helpers |
| `arreio-vault` | Secure secret storage + encryption + rotação automática persistida (`AutoRotator`, PVC-Q3.2) |
| `arreio-contract` | Design by Contract assertions + Hoare-style pre/post-conditions |
| `arreio-supercompile` | Supercompilation / partial evaluation pipeline |
| `arreio-reasoning` | Reasoning como serviço auditável: CoT/ToT/ReAct/PAL com budget e ledger hash-chain (PVC-Q2.1) |
| `arreio-commissioning` | Self-Commissioning: StubDetector + BriefGenerator + ReportGenerator determinísticos (PVC-Q3.3) |

### Bridge, protocol & benchmark crates

| Crate | Role |
|---|---|
| `arreio-a2a` | Google A2A protocol adapter: Agent Card, task lifecycle, artifacts, SSE streaming |
| `arreio-mcp-server` | MCP server exposing O Arreio as a tool provider over stdio/HTTP/SSE |
| `arreio-bridge-claude` | MCP stdio bridge for Claude Code / Claude Desktop |
| `arreio-bridge-cursor` | HTTP + SSE bridge for Cursor IDE (emulates composer API) |
| `arreio-bridge-hermes` | Bidirectional A2A + OpenAI-compatible bridge for Hermes Agent |
| `arreio-bridge-openclaw` | REST JSON → DAG bridge for OpenClaw gateway |
| `arreio-benchmark` | Benchmark framework: provider latency, Blackboard throughput, SYMBION metrics |

### SYMBION expansion crates (10 subsystems)

| Crate | Subsistema SYMBION | Role |
|---|---|---|
| `arreio-ooda` | OODA-C Control Loop | Observe-Orient-Decide-Act with artificial homeostasis, IG&C and Ashby essential variables |
| `arreio-problem-space` | Problem Space Engine | Newell & Simon problem-space hypothesis + SOAR universal subgoaling for automatic impasse recovery |
| `arreio-recovery` | Recovery Block Multi-Model | Fault tolerance via diverse LLMs: `ensure <test> by <primary> else by <alt>` |
| `arreio-slicer` | Context Curation (Program Slicing) | Weiser-style backward/forward slicing to minimize context sent to the LLM |
| `arreio-refinement` | Refinement-Based Generation | Formal refinement calculus: specification → refinement steps → correct-by-construction code |
| `arreio-autopoiesis` | Autopoietic Sustainability | MAPE-K self-regulation + Maturana & Varela autopoiesis for self-healing |
| `arreio-eqsat` | Equality Saturation | e-graph based non-destructive optimization (Turing/egg); unifies supercompilation and peephole opts |

## Key architecture decisions

**Blackboard replaces message-passing.** Actors never call each other — they read/write the single `Blackboard`. This eliminates conversation-history accumulation and keeps each actor invocation stateless.

**No tokio / no async.** Everything is synchronous. The hypervisor uses a poll loop for timeouts. This keeps the dependency tree small and avoids build-script chains.

**Raw TCP for Ollama.** `OllamaClient` opens a plain `TcpStream` to `127.0.0.1:11434`. No TLS, no `reqwest`, no ICU dependency chain that would require build scripts.

**Token economy.** Actors receive: (1) a 2-3 sentence system prompt, (2) one task JSON tuple, (3) an optional compact AST symbol map. No history. Symbol maps use `to_compact_json()` (no whitespace) to minimize injected tokens.

**`vendor/zmij` patch.** `serde_json ≥1.0.149` depends on the `zmij` float-printer crate whose `build.rs` is blocked by AppControl. The workspace patches it with a local copy at `vendor/zmij/` that has no `build.rs` (safe on rustc ≥1.88).

**FSM states:** `Idle → Exploration → Planning → Execution → Evaluation → Correction → Consolidation`; `StrategicRetreat` is only reachable via `Watchdog::interrupt()`.

**Checkpoint flow:** before each DAG node executes, `Checkpoint::save` runs `git add -A && git commit`. On failure, `Checkpoint::rollback` runs `git reset --hard HEAD~1`.

## Integrated Execution Pipeline (SYMBION)

The full pipeline layers the ten SYMBION subsystems over the original FSM → Actor → DAG flow:

1. **OODA-C Layer** (`arreio-ooda`) — evaluates essential variables (working-tree integrity, dependency consistency, confidence score) before any destructive action. If variables are within bounds, IG&C (Implicit Guidance & Control) permits fast-path execution without deliberation.
2. **Problem Space Layer** (`arreio-problem-space`) — decomposes the user request into states, operators and subgoals. When an impasse occurs (state no-change, operator tie/conflict, rejection), universal subgoaling spawns automatic subgoals.
3. **Context Curation** (`arreio-slicer`) — given a target symbol or criterion, produces the minimal backward/forward program slice so only relevant code is injected into the LLM prompt.
4. **Contract Layer** (`arreio-contract`) — derives pre-conditions, post-conditions and invariants from the natural-language specification. These travel with every generated function.
5. **Refinement / Generation** (`arreio-refinement`) — for critical paths, translates the specification into a chain of refinement steps (weaken precondition, strengthen postcondition, assignment, alternation, iteration) and generates code guaranteed to satisfy the contract.
6. **Recovery Layer** (`arreio-recovery`) — executes the primary model; if the acceptance test fails, falls back to alternate models (`else by <alt>`) until success or exhaustion.
7. **Equality Saturation** (`arreio-eqsat`) — optimizes generated code via e-graph rewriting without losing semantic equivalence; feeds back into the AST symbol map.
8. **Autopoiesis** (`arreio-autopoiesis`) — MAPE-K loop monitors system health, repairs degraded components, and triggers self-healing actions via the Blackboard.

For routine tasks the pipeline short-circuits: OODA-C fast-path → direct DAG execution → checkpoint. The full ten-layer stack is engaged only when confidence scores drop or impasses are detected.

## Empirical Feedback Loop (Continual Harness absorption)

The system now includes a closed-loop empirical feedback mechanism inspired by Continual Harness (Karten et al., 2026):

**TrajectoryStore** (`arreio-kernel/src/trajectory.rs`) — Records every DAG node execution result (success/failure/timeout/blocked) in the Blackboard as structured `TrajectoryEntry` tuples. Prunes to 1000 entries max. Provides `recent()` window queries for the Refiner.

**Refiner (4th actor)** (`arreio-actors/src/refiner.rs`) — Runs every 10 DAG nodes. Reads the trajectory window, detects contracts that failed ≥3 times with ≥2 different models, and either re-derives contracts via LLM or escalates to human operator.

**AdaptiveCadence** (`arreio-skills/src/learn.rs`) — Prevents skill pollution by throttling auto-learning: 10-task warmup, frequent learning in early phase (every 5 tasks), sparse in stable phase (every 25 tasks).

**MutationHistory** (`arreio-skills/src/store.rs`) — Tracks every field-level change to Skill entries (timestamp, source, old/new values) for audit and rollback.

**SkillValidator** (`arreio-skills/src/validator.rs`) — 6-check pipeline run on every skill before storage: anti-conversation, idempotent, error_budget, output_schema, allowed_tools, trust_level.

**Skill CRUD tools** (`arreio-tools/src/skill_crud.rs`) — Developer can create/update/delete skills during the tool-use loop, with automatic validation and trust gating (new skills born `Untrusted`).

**Enriched ActorContext** (`arreio-actors/src/actors.rs`) — 5 new fields for multi-step coherence: `architect_rationale`, `dependencies_summary`, `parent_spec`, `retry_context` (RetryContext struct with attempt_number, max_attempts, previous_errors, models_tried), and `trajectory_window`.

## Ollama configuration

Default model: `gemma4:latest` at `127.0.0.1:11434`. The model name is passed at runtime via the `run` command; actors don't hard-code it.

---

## O Arreio como Hub Universal

O Arreio é posicionado como **infraestrutura de orquestração**, nunca como substituto de editores ou IDEs. Ele atua como um hub universal que conecta agentes, modelos e ferramentas através de protocolos abertos, permitindo que qualquer cliente (Claude Code, Cursor, Hermes, OpenClaw, etc.) consuma seus serviços sem acoplar-se à implementação interna.

Princípios do hub:
- **Protocolo sobre produto**: toda integração exposta via protocolo standardizado (MCP, A2A, REST).
- **Stateless por padrão**: o estado reside no Blackboard; clientes enviam apenas o contexto mínimo.
- **Substituibilidade**: qualquer subsistema pode ser trocado sem quebrar contratos externos.

## MCP Server de Infraestrutura

O crate `arreio-mcp-server` expõe o Arreio como um servidor MCP (Model Context Protocol) puro sobre stdio/SSE. Em vez de apenas consumir ferramentas externas, o Arreio **provê** ferramentas para clientes MCP:

- `arreio_blackboard_read` — consulta tuplas do Blackboard por padrão ou chave.
- `arreio_blackboard_write` — publica uma tupla tipada no Blackboard.
- `arreio_dag_schedule` — recebe um grafo de tarefas JSON e retorna o scheduling topológico.
- `arreio_skill_invoke` — executa uma skill do Tuple Space por nome, com argumentos JSON.
- `arreio_checkpoint_save / arreio_checkpoint_rollback` — gerencia checkpoints git via MCP.

O servidor é síncrono (sem tokio) e usa parsing manual de JSON-RPC 2.0, alinhado à arquitetura existente.

## A2A Compatibility

O crate `arreio-a2a` implementa compatibilidade com o protocolo A2A (Agent-to-Agent) da Google, permitindo que agentes O Arreio se comuniquem com agentes externos via trocas de mensagens estruturadas:

- **Agent Card**: descritor JSON publicado em `/.well-known/agent.json` com capabilities, skills e endpoint.
- **Task Lifecycle**: mapeia estados A2A (`submitted`, `working`, `input-required`, `completed`, `failed`, `canceled`) para estados da FSM O Arreio.
- **Artifacts**: resultados de tarefas são convertidos em artefatos A2A (MIME-typed) e vice-versa.
- **Streaming**: suporte a updates parciais via SSE, traduzidos para o pub/sub do Blackboard.

A integração A2A é stateless: o estado da conversação A2A é serializado como uma tupla no Blackboard, não mantido em memória do adapter.

## Multi-Provider com TLS

O crate `arreio-provider` evoluiu de cliente TCP puro para suportar múltiplos provedores com TLS nativo (sem `reqwest`, usando `rustls` com `ring`):

| Provedor | Transporte | Autenticação | Endpoint padrão |
|---|---|---|---|
| Ollama | TCP puro | Nenhuma | `127.0.0.1:11434` |
| OpenAI | TLS 1.3 | Bearer token (`OPENAI_API_KEY`) | `api.openai.com/v1` |
| Anthropic | TLS 1.3 | `x-api-key` header (`ANTHROPIC_API_KEY`) | `api.anthropic.com` |
| Google (Gemini) | TLS 1.3 | API key em query param (`GOOGLE_API_KEY`) | `generativelanguage.googleapis.com` |
| Azure OpenAI | TLS 1.3 | Bearer token (`AZURE_OPENAI_API_KEY` + `AZURE_OPENAI_ENDPOINT`) | `{resource}.openai.azure.com` |
| DeepSeek | TLS 1.3 | Bearer token (`DEEPSEEK_API_KEY`) | `api.deepseek.com` |
| Kimi (Moonshot) | TLS 1.3 | Bearer token (`MOONSHOT_API_KEY`) | `api.moonshot.ai/v1` |
| MiniMax | TLS 1.3 | Bearer token (`MINIMAX_API_KEY`) | `api.minimax.io/v1` |
| OpenRouter | TLS 1.3 | Bearer token (`OPENROUTER_API_KEY`) | `openrouter.ai/api/v1` |

Uso no CLI: `arreio run --model kimi:kimi-k2.5`, `minimax:<modelo>`, `openrouter:<vendor/modelo>` — mesmo padrão `provider:modelo` dos demais. Kimi, MiniMax e OpenRouter reutilizam `OpenAiCompatProvider` (construtores `kimi()`, `minimax()`, `openrouter()` com `base_path` e rótulo próprios).

Regras de implementação:
- Cada provedor é um módulo separado em `arreio-provider/src/`.
- A construção do `TcpStream` e do TLS connector é unificada em `arreio-provider/src/transport.rs`.
- O cliente seleciona o provedor via enum em runtime; a chave de API é injetada a partir do Vault, nunca hardcoded.
- Retry exponencial (1s → 2s → 4s) e métricas de tokens continuam aplicáveis a todos os provedores.

## API Key Vault (AES-256-GCM)

O crate `arreio-vault` gerencia segredos com criptografia AES-256-GCM via `ring`:

- **Master key**: derivada de uma passphrase via Argon2id (configuração OWASP: 64 MB, 3 iterações, 1 paralelismo).
- **Key wrapping**: cada API key é cifrada com a master key e armazenada em `~/.arreio/vault/keys.json`.
- **Access control**: leitura do Vault requer a capability `vault:read:<prefix>` publicada no Blackboard.
- **Rotation**: suporte a múltiplas versões da mesma chave; a versão ativa é indicada por uma tupla no Tuple Space.
- **Zeroização**: buffers de chave são explicitamente zerados após uso (via `zeroize`).

O Vault nunca persiste a master key em disco; ela é solicitada via prompt TUI (`arreio-tui`) ou variável de ambiente `ARREIO_MASTER_KEY` (último recurso, com warning).

## Bridge / Adapters

O diretório `crates/` inclui crates `arreio-bridge-*` que adaptam o Arreio para consumo por ferramentas e agentes específicos:

| Crate | Ferramenta | Protocolo | Função |
|---|---|---|---|
| `arreio-bridge-claude` | Claude Code / Claude Desktop | MCP client | Expõe skills O Arreio como tools MCP; traduz respostas em XML tags `<thinking>` / `<action>` |
| `arreio-bridge-cursor` | Cursor | HTTP + SSE | Servidor local que emula a API de compositor do Cursor, injetando skills O Arreio no contexto |
| `arreio-bridge-hermes` | Hermes Agent | A2A | Adapter bidirecional A2A; Hermes publica tarefas no Blackboard e recebe updates via SSE |
| `arreio-bridge-openclaw` | OpenClaw | REST custom | Mapeia endpoints OpenClaw para tuplas do Blackboard; converte planos JSON em DAGs |

Convenções de bridge:
- Cada bridge é um crate independente com seu próprio `Cargo.toml`.
- Bridges nunca acessam diretamente o estado interno de outro crate — apenas leem/escrevem no Blackboard.
- Testes unitários de bridge usam um Blackboard em memória (`Blackboard::temp()`) para isolamento.

## SQLite Persistence

Além da persistência JSON do Blackboard, o crate `arreio-kernel` suporta backend SQLite para casos que exigem consultas estruturadas e transacionais:

- **Modo híbrido**: o Blackboard mantém o Tuple Space em memória; periodicamente (ou sob demanda) sincroniza para SQLite via `VACUUM INTO` equivalente.
- **Esquema**: tabelas `tuples` (id, topic, payload_json, timestamp, ttl), `events` (id, channel, payload_json, timestamp) e `snapshots` (id, blackboard_json, created_at).
- **Ativação**: configurável via variável de ambiente `ARREIO_PERSISTENCE=sqlite` ou `json` (padrão).
- **Vantagens**: queries SQL para auditoria, TTL automático de tuplas expiradas, WAL mode para concorrência.
- **Limitação**: o SQLite é um backend secundário; o caminho crítico (pub/sub, Tuple Space) continua em memória para latência mínima.

## DLP e Segurança Enterprise

O crate `arreio-security` incorpora controles de Data Loss Prevention (DLP) e hardening enterprise:

- **DLP Scanner**: regex e heurísticas para detectar dados sensíveis em tuplas antes de envio a provedores LLM:
  - CPF/CNPJ, cartões de crédito (Luhn), e-mails corporativos.
  - Chaves AWS (`AKIA...`), tokens GitHub (`ghp_...`), chaves privadas PEM.
  - PII em código-fonte gerado (endereços, telefones).
- **Redaction automática**: quando dados sensíveis são detectados, o scanner substitui por `[REDACTED:<tipo>]` e publica um evento `security:dlp_alert` no Blackboard.
- **Content Safety**: integração com classificadores locais (via Ollama) para detectar prompts de injeção, jailbreak e solicitação de código malicioso antes do envio ao modelo.
- **Audit Log**: todas as ações do Hypervisor (comandos executados, bloqueios, rollbacks) e todas as leituras/escritas no Vault são registradas em `~/.arreio/audit/YYYY-MM.log` (append-only, com hash em cadeia SHA-256).
- **Enterprise Hardening**:
  - Suporte a execução em modo `read-only` (nenhum comando de shell é executado; apenas análise estática).
  - Policy engine: regras JSON configuráveis (`~/.arreio/policies.json`) que definem blocklist adicional, allowed file paths e rate limits por ator.

## Metodologia PVC e Artefatos de Referência

Este projeto segue a metodologia **PVC (Protótipo Vertical Comissionável)**. Todo trabalho de longa duração deve respeitar os Gates G0-G9 e usar os artefatos abaixo como fonte de verdade.

| Artefato | Caminho | Quando usar |
|---|---|---|
| **Hub PVC** | [`handoffs_spec-driven-devolopment.PVC/README.md`](./handoffs_spec-driven-devolopment.PVC/README.md) | Estado atual dos gates, métricas, roadmap de inovações, próximo PVC |
| **Síntese Mercado × Arquitetura** | [`handoffs_spec-driven-devolopment.PVC/research/08-sintese-mercado-arquitetura-omni-os/README.md`](./handoffs_spec-driven-devolopment.PVC/research/08-sintese-mercado-arquitetura-omni-os/README.md) | Decisões arquiteturais, posicionamento competitivo, o que vale traduzir do mercado |
| **Metodologia** | [`../metodologia_pvc_padroes_para_vibecoding_ia.md`](../metodologia_pvc_padroes_para_vibecoding_ia.md) | Padrões, templates, checklists, fluxo operacional completo |
| **Plano Atual** | [`handoffs_spec-driven-devolopment.PVC/proximos planos/01-execucao-curto-prazo/00-README.md`](./handoffs_spec-driven-devolopment.PVC/proximos%20planos/01-execucao-curto-prazo/00-README.md) | PVC-M1/M2/M3 (90 dias): publicar, ser usado por estranhos, decidir com dados |

> **Regra PVC:** Pequeno pode. Simulado pode temporariamente. Incompleto oculto não pode.
>
> Antes de implementar: verifique `MOCK_REGISTER.md`. Após implementar: atualize `COMMISSIONING_REPORT.md`. Nova decisão arquitetural: crie ADR em `adr/`.
