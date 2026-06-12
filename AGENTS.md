# O Arreio — Guia para Agentes de Código

> Este arquivo é destinado a agentes de IA que trabalham no código-fonte do projeto Arreio. O leitor deve ser tratado como alguém que não conhece nada sobre o projeto.

---

## Visão Geral do Projeto

**O Arreio** é um "Sistema Operacional para LLMs" escrito em Rust. Ele orquestra agentes de IA (Arquiteto, Desenvolvedor, Inspetor) usando padrões clássicos de ciência da computação: Blackboard (HEARSAY-II), Máquinas de Estado Finito (FSM), Modelo de Atores, Grafos Acíclicos Dirigidos (DAG) e Espaço de Tuplas.

O objetivo é substituir a abordagem monolítica de agentes conversacionais por uma arquitetura distribuída, stateless e rigorosamente controlada, onde a IA atua apenas como motor de inferência e o *harness* gerencia estado, segurança, memória e execução.

O projeto reside no diretório `arreio/`. Todos os comandos de build devem ser executados a partir de `arreio/`.

---

## Stack Tecnológico

- **Linguagem**: Rust (edition 2021)
- **Toolchain alvo**: `stable-x86_64-pc-windows-gnu`
- **Toolchain C**: MSYS2 UCRT64 (`gcc`, `dlltool`, `ar`)
- **Build**: Cargo com workspace de múltiplos crates
- **Persistência**: JSON em arquivo (Blackboard) ou SQLite (opcional)
- **LLM local**: Ollama via TCP puro (raw HTTP/1.0 sobre `TcpStream`)
- **LLM remoto**: OpenAI, Anthropic, Google, Azure, DeepSeek, Kimi (Moonshot), MiniMax e OpenRouter via TLS nativo (`native-tls`)
- **Modelo padrão**: `gemma4:latest` em `127.0.0.1:11434`
- **Comentários e documentação**: Português

---

## Estrutura do Workspace

### Core crates

| Crate | Responsabilidade | Dependências principais |
|---|---|---|
| `arreio-kernel` | Blackboard (estado central compartilhado) + Tuple Space + Pub/Sub, persistido em JSON/SQLite | `serde`, `serde_json`, `uuid`, `anyhow`, `thiserror` |
| `arreio-fsm` | Máquina de Estado Finito com 9 estados; estado persistido no Blackboard | `arreio-kernel` |
| `arreio-actors` | Atores: Arquiteto, Desenvolvedor, Inspetor, **Refiner** + ActorContext enriquecido (RetryContext, trajectory_window) + cliente Ollama TCP cru | `arreio-kernel`, `arreio-fsm` |
| `arreio-hypervisor` | Sandbox de processos: interceptor de comandos + watchdog de detecção de loops | `arreio-kernel`, `regex` |
| `arreio-dag` | Motor de DAG: scheduling topológico + checkpoints baseados em git | `arreio-kernel` |
| `arreio-ast` | Extrator de mapa de símbolos Rust (via `syn`) + fallback regex para outras linguagens | `syn`, `quote` |
| `arreio-provider` | Cliente multi-provedor LLM (Ollama TCP, OpenAI/Anthropic/Google/Azure TLS puro) | `arreio-kernel`, `rustls`, `ring` |
| `arreio-memory` | Motor de memória: envelopes tipados, GraphStore, recall híbrido, lifecycle | `arreio-kernel`, `regex` |
| `arreio-skills` | Skill Store + auto-learning adaptativo + SkillValidator (6 checks) + MutationHistory + matching por triggers | `arreio-kernel`, `arreio-memory`, `arreio-ast` |
| `arreio-gateway` | Servidor HTTP síncrono + API REST + dashboard SPA embutido | `arreio-kernel`, `arreio-fsm`, `arreio-dag` |
| `arreio-lsp` | Cliente LSP JSON-RPC 2.0 sobre stdio | `serde`, `serde_json` |
| `arreio-cli` | Ponto de entrada CLI (`clap`): `init`, `run`, `resume`, `serve`, `status`, `rollback`, `skills` | todos os crates acima |

### Novos crates de infraestrutura e integração

| Crate | Responsabilidade | Dependências principais |
|---|---|---|
| `arreio-a2a` | Adapter Agent-to-Agent (Google A2A protocol): Agent Card, task lifecycle, artifacts, SSE streaming | `arreio-kernel`, `arreio-fsm` |
| `arreio-mcp-server` | Servidor MCP (Model Context Protocol) que expõe o Arreio como provider de tools | `arreio-kernel`, `arreio-skills`, `arreio-dag` |
| `arreio-bridge-claude` | Bridge para Claude Code / Claude Desktop (MCP client, XML tags) | `arreio-kernel`, `arreio-mcp-server` |
| `arreio-bridge-cursor` | Bridge para Cursor (HTTP + SSE, emula API de compositor) | `arreio-kernel`, `arreio-gateway` |
| `arreio-bridge-hermes` | Bridge para Hermes Agent (A2A bidirecional) | `arreio-kernel`, `arreio-a2a` |
| `arreio-bridge-openclaw` | Bridge para OpenClaw (REST custom, JSON → DAG) | `arreio-kernel`, `arreio-dag` |
| `arreio-benchmark` | Framework de benchmark + EvalSets estruturados com detecção de regressão >5% (PVC-Q2.2) | `arreio-kernel`, `arreio-provider` |
| `arreio-reasoning` | Reasoning como serviço auditável: PromptMode (CoT/ToT/ReAct/PAL), budget explícito, ledger com hash SHA-256 encadeado (PVC-Q2.1) | `arreio-kernel`, `arreio-fsm`, `arreio-provider`, `arreio-memory` |
| `arreio-commissioning` | Self-Commissioning determinístico: StubDetector, BriefGenerator, ReportGenerator com decisão calculada de evidências (PVC-Q3.3) | `arreio-kernel` |

### SYMBION expansion crates (10 subsystems)

| Crate | Subsistema SYMBION | Role |
|---|---|---|
| `arreio-ooda` | OODA-C Control Loop | Observe-Orient-Decide-Act com homeostase artificial |
| `arreio-problem-space` | Problem Space Engine | Newell & Simon + SOAR universal subgoaling |
| `arreio-recovery` | Recovery Block Multi-Model | Fault tolerance via LLMs diversos |
| `arreio-slicer` | Context Curation (Program Slicing) | Weiser-style slicing para minimizar contexto |
| `arreio-refinement` | Refinement-Based Generation | Refinement calculus correct-by-construction |
| `arreio-autopoiesis` | Autopoietic Sustainability | MAPE-K self-regulation + self-healing |
| `arreio-eqsat` | Equality Saturation | e-graph based non-destructive optimization |
| `arreio-security` | Security & DLP | Policies, credential scanning, auth guardrails |
| `arreio-telemetry` | Metrics & Tracing | Blackboard-based observability |
| `arreio-vault` | Secure Secret Storage | AES-256-GCM + Argon2id |

---

## Configuração do Ambiente de Build

Antes de qualquer comando `cargo`, certifique-se de que ambos os toolchains estão no `PATH`:

```powershell
$env:PATH = "$env:USERPROFILE\.cargo\bin;C:\msys64\ucrt64\bin;$env:PATH"
```

O arquivo `.cargo/config.toml` redireciona os artefatos de build para `C:\dev\omni-target` (fora do OneDrive, onde o Windows AppControl bloqueia binários compilados). Também define o linker e o `ar` do MSYS2.

---

## Comandos de Build e Teste

Execute sempre a partir de `arreio/`:

```bash
# Verificação rápida de tipos em todos os crates
cargo check --workspace

# Rodar todos os testes unitários
cargo test --workspace

# Rodar testes de um crate específico (útil quando o AppControl bloqueia um binário recém-compilado — basta tentar novamente)
cargo test -p arreio-kernel
cargo test -p arreio-fsm
cargo test -p arreio-actors
cargo test -p arreio-hypervisor
cargo test -p arreio-dag
cargo test -p arreio-ast
cargo test -p arreio-provider
cargo test -p arreio-security
cargo test -p arreio-vault
cargo test -p arreio-cli

# Build do binário CLI
cargo build --bin arreio

# Testes de fumaça do CLI
cargo run --bin arreio -- init
cargo run --bin arreio -- status
cargo run --bin arreio -- skills
cargo run --bin arreio -- serve --port 7373
```

**Nota sobre Windows AppControl**: se um binário de teste for bloqueado na primeira execução, tente rodar o teste do crate específico novamente. Isso é um comportamento conhecido do ambiente.

---

## Convenções para Desenvolvimento de Adapters

Adapters (crates `arreio-bridge-*` e `arreio-a2a`) seguem regras rigorosas para manter o Arreio desacoplado:

1. **Isolamento de estado**: adapters nunca mantêm estado próprio em memória entre requisições. Tudo é lido/escrito no Blackboard.
2. **Interface mínima**: cada adapter expõe no máximo duas funções públicas — `start(listener)` e `shutdown()`.
3. **Erros traduzidos**: erros do protocolo externo são convertidos para `arreio_kernel::ArreioError` antes de propagar.
4. **Testes com Blackboard temporário**: testes usam `Blackboard::temp()` para isolamento completo.
5. **Sem async**: adapters usam threads std (`std::thread::spawn`) e channels (`crossbeam-channel`) para concorrência.
6. **Timeouts defensivos**: toda chamada a cliente externo tem timeout configurável (padrão 30s).

Template mínimo para novo adapter:

```rust
// crates/arreio-bridge-<nome>/src/lib.rs
use arreio_kernel::{Blackboard, ArreioError, Result};
use std::thread;
use crossbeam_channel::{bounded, Sender, Receiver};

pub struct Bridge<N> {
    bb: Blackboard,
    cmd_tx: Sender<BridgeCmd>,
}

enum BridgeCmd { Shutdown }

impl<N> Bridge<N> {
    pub fn new(bb: Blackboard) -> Result<Self> { ... }
    pub fn start(&self, listener: impl Fn(Event) -> Result<()>) -> Result<()> { ... }
    pub fn shutdown(&self) -> Result<()> { ... }
}
```

---

## Como Adicionar Novo Provider LLM

Para adicionar um novo provedor ao `arreio-provider`:

1. **Crie o módulo**: `crates/arreio-provider/src/<nome>.rs`.
2. **Implemente o trait `LlmProvider`**:
   ```rust
   pub trait LlmProvider {
       fn complete(&self, prompt: &Prompt) -> Result<Completion>;
       fn stream(&self, prompt: &Prompt) -> Result<impl Iterator<Item = Result<Chunk>>>;
       fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
   }
   ```
3. **Transporte**: use `crate::transport::TlsConnector` para endpoints HTTPS, ou `TcpStream` puro para endpoints locais.
4. **Autenticação**: injete a API key via `arreio_vault::Vault::get_key(prefix)` — nunca aceite string literal no construtor.
5. **Registre no enum**: adicione a variante em `crates/arreio-provider/src/lib.rs` no `ProviderConfig`.
6. **Testes**: adicione teste unitário que valide parsing da resposta a partir de JSON fixture (sem chamada de rede).

Exemplo de fixture esperada em `crates/arreio-provider/src/fixtures/<nome>_completion.json`.

---

## Como Adicionar Novo Adapter de Ferramenta

Para criar um novo adapter (ex: `arreio-bridge-vscode`):

1. **Copie o template**: use `arreio-bridge-hermes` como referência.
2. **Defina o protocolo**: mapeie as operações do cliente para tuplas do Blackboard.
3. **Implemente `ToolAdapter`**:
   ```rust
   pub trait ToolAdapter {
       fn name(&self) -> &str;
       fn capabilities(&self) -> Vec<Capability>;
       fn handle_request(&self, req: Request, bb: &Blackboard) -> Result<Response>;
   }
   ```
4. **Pub/Sub**: se o protocolo suportar notificações, traduza para `bb.subscribe(channel)`.
5. **Segurança**: valide todo input externo com `arreio_security::InputValidator` antes de escrever no Blackboard.
6. **Documente**: adicione uma linha na tabela de Bridges deste arquivo.

---

## Regras de Segurança para Contribuições

Toda contribuição ao Arreio deve respeitar as seguintes regras:

### Código gerado / modificado
- **Nunca** hardcode credenciais, tokens ou chaves de API — mesmo em exemplos ou testes de integração. Use `arreio_vault::Vault::get_key("test")` com chave dummy.
- **Nunca** adicione dependências que executem `build.rs` arbitrário sem revisão manual. O patch `vendor/zmij` existe para evitar exatamente isso.
- **Nunca** use `unsafe` sem comentário justificativo e sem teste de sanitizer (`cargo test` já roda com MIRI quando disponível).

### Hypervisor e execução
- Todo novo comando ou ferramenta invocada pelo Hypervisor deve ser adicionada à blocklist do `arreio-hypervisor/src/interceptor.rs` **antes** de ser liberada.
- Comandos que alteram o sistema de arquivos fora do diretório de trabalho exigem flag `--dangerous` explícita no CLI.

### DLP e dados sensíveis
- Todo texto que transita do Blackboard para um provedor LLM deve passar por `arreio_security::dlp::Scanner::scan()`.
- Se um teste precisar incluir dados que pareçam PII, use o prefixo `FAKE_` (ex: `FAKE_CPF=00000000000`).

### Audit e compliance
- Ações administrativas (leitura de Vault, rollback, alteração de policy) devem emitir evento `audit:*` no Blackboard.
- Logs de audit são append-only; nunca truncados ou deletados pelo código do Arreio.

### Revisão obrigatória
- Mudanças em `arreio-security`, `arreio-vault`, `arreio-hypervisor` ou qualquer crate `arreio-bridge-*` exigem:
  1. Teste unitário cobrindo o caso de uso.
  2. Teste de regressão de segurança (ex: tentativa de injeção de comando).
  3. Atualização deste `AGENTS.md` se houver mudança de interface ou convenção.

---

## Instruções de Teste

- Todos os crates possuem testes unitários cobrindo funcionalidade principal e edge cases.
- Os testes do `arreio-kernel` validam concorrência (50 threads escrevendo simultaneamente no Blackboard) e persistência SQLite.
- Os testes do `arreio-fsm` validam transições válidas e inválidas, incluindo interrupção e reset.
- Os testes do `arreio-hypervisor` validam bloqueio de comandos destrutivos, execução segura e timeout.
- Os testes do `arreio-dag` validam detecção de ciclos, scheduling topológico e persistência.
- Os testes do `arreio-ast` validam extração de funções, tipos, e que o JSON compacto é menor que o código-fonte.
- Os testes do `arreio-actors` validam extração de JSON de respostas com markdown fences e desserialização de resultados de inspeção.
- Os testes do `arreio-provider` validam parsing de respostas de todos os provedores a partir de fixtures JSON.
- Os testes do `arreio-security` validam detecção de credenciais, redaction e falsos positivos.
- Os testes do `arreio-vault` validam criptografia, derivação de chave e zeroização.

Para rodar testes de um crate isolado (útil em caso de bloqueio do AppControl):
```bash
cargo test -p <nome-do-crate>
```

---

## Considerações de Segurança

- **Interceptor de comandos**: o `Hypervisor` valida todo comando de shell contra uma blocklist regex antes de executar. Comandos bloqueados incluem: `rm -rf`, `format`, `chmod 777`, `curl | sh`, `DROP DATABASE`, etc.
- **Timeout de execução**: o `Hypervisor` usa poll loop com timeout configurável (padrão 30s no CLI). Processos que excedem o timeout recebem kill e retornam exit code `-2`.
- **Watchdog de loops**: se o mesmo `exit_code` ocorrer N vezes consecutivas (padrão 3), o `Watchdog` publica um evento de `interrupt` no Blackboard, forçando a FSM para `StrategicRetreat`.
- **Inspetor de segurança**: todo código gerado pelo Desenvolvedor passa pelo Inspetor, que bloqueia: injeção de comandos, credenciais hardcoded, remoção de autenticação, SQL injection e loops infinitos sem escape.
- **DLP**: antes de enviar qualquer tupla a um provedor LLM remoto, o scanner DLP verifica dados sensíveis e aplica redaction automática.
- **Vault**: a master key nunca persiste em disco; a zeroização de buffers é obrigatória após uso.
- **Sem acesso de rede externo não autorizado**: o cliente Ollama conecta apenas a `127.0.0.1:11434`. Provedores remotos usam TLS 1.3 com verificação de certificado.
- **Rollback automático**: falhas em validação (build/test) disparam `git reset --hard HEAD~1` para reverter alterações no disco.
- **Audit log**: todas as ações críticas são registradas em `~/.arreio/audit/` com hash em cadeia SHA-256.

---

## Notas sobre o Modelo LLM

- O nome do modelo é passado em runtime via argumento `--model` no comando `run`. O padrão é `gemma4`.
- Os atores não hard-codam o nome do modelo; recebem via construtor.
- O Ollama deve estar rodando localmente em `127.0.0.1:11434` antes de executar `arreio run`.
- Para provedores remotos (OpenAI, Anthropic, Google, Azure), a chave de API é obtida do Vault no momento da execução.

---

## Dependências do Workspace

As dependências são centralizadas no `Cargo.toml` raiz:

| Crate | Versão | Observação |
|---|---|---|
| `serde` | `1` (derive) | Serialização universal |
| `serde_json` | `1` | Patch `vendor/zmij` resolve build.rs bloqueado |
| `crossbeam-channel` | `0.5` | Channels MPMC síncronos (paralelismo DAG sem async) |
| `num_cpus` | `1` | Detecção de cores para thread pool |
| `uuid` | `1` (v4) | IDs de eventos |
| `anyhow` | `1` | Erros dinâmicos |
| `thiserror` | `1` | Erros tipados customizados |
| `regex` | `1` | Blocklist do interceptor + DLP scanner |
| `syn` | `2` (full, parsing) | Parser Rust para AST |
| `quote` | `1` | Geração de código para AST |
| `clap` | `4` (derive) | CLI |
| `native-tls` | `0.2` | TLS para provedores remotos |
| `aes-gcm` | `0.10` | Criptografia simétrica AES-256-GCM |
| `pbkdf2` | `0.12` | Derivação de chave (PBKDF2) |
| `zeroize` | `1` | Zeroização de buffers de segredo |
| `rusqlite` | `0.32` | Persistência SQLite (opcional) |

Dependências de desenvolvimento: `tempfile = "3"` (usada em testes de múltiplos crates).

---

## Empirical Feedback Loop (absorção do Continual Harness)

Baseado no padrão de loop empírico do Continual Harness (Karten et al., 2026), o Arreio agora possui um ciclo fechado de feedback:

- **TrajectoryStore** (`arreio-kernel/src/trajectory.rs`) — Registra todo resultado de execução de nó DAG (success/failure/timeout/blocked) no Blackboard. Janela de 1000 entradas com pruning automático.
- **Refiner (4º ator)** (`arreio-actors/src/refiner.rs`) — Executa a cada 10 nós. Detecta contratos que falharam ≥3 vezes com ≥2 modelos diferentes e re-deriva contratos via LLM ou escala para operador humano.
- **AdaptiveCadence** (`arreio-skills/src/learn.rs`) — Throttling de auto-aprendizado: warmup de 10 tarefas, frequente no início (a cada 5), espaçado na fase estável (a cada 25).
- **MutationHistory** (`arreio-skills/src/store.rs`) — Rastreia toda alteração campo-a-campo em Skills (timestamp, origem, valores old/new) para auditoria e rollback.
- **SkillValidator** (`arreio-skills/src/validator.rs`) — Pipeline de 6 verificações executado em toda skill antes de armazenamento.
- **Skill CRUD tools** (`arreio-tools/src/skill_crud.rs`) — Ferramentas de create/update/delete de skills disponíveis para o Developer durante o tool-use loop, com validação automática e trust gating.

## Protocolo AXON (Status: Referência Externa)

O diretório `protocolo/reference/rust/` contém uma implementação do protocolo AXON em 5 camadas:

| Layer | Componente | Estado | Nota |
|---|---|---|---|
| L0 — Framing | MessagePack frame parser | ✅ Funcional | Fora do workspace O Arreio |
| L1 — Transport | WebSocket client/server | ✅ Funcional | Usa `tokio` (async) |
| L2 — Security | mTLS, JWT, DLP, audit chain | ✅ Funcional | Não integrado ao runtime |
| L3 — Semantics | AgentCard, Task, State, Tool | ❌ Stub vazio | FASE 2+ do roadmap |
| L4 — Orchestration | Distributed DAG, coordinator | ❌ Stub vazio | FASE 3+ do roadmap |

**Decisão arquitetural**: AXON NÃO faz parte do runtime O Arreio na FASE 0–1. Ele é mantido como protótipo de pesquisa porque:
1. Usa `tokio` (async), enquanto Arreio é deliberadamente síncrono (threads POSIX)
2. As camadas L3–L4 ainda são structs vazios sem comportamento
3. Integração requer decisão sobre async no projeto — adiada para FASE 3+

Crates do workspace O Arreio não devem depender de AXON. Referências acidentais (ex: `arreio-provider/src/error_classifier.rs`) devem ser removidas.

## Metodologia PVC e Documentação de Referência

Este projeto segue a metodologia **PVC (Protótipo Vertical Comissionável)**. Todo agente de código deve consultar os artefatos abaixo antes de iniciar tarefas de longa duração.

| Artefato | Caminho | Quando usar |
|---|---|---|
| **Hub PVC (estado do projeto)** | [`handoffs_spec-driven-devolopment.PVC/README.md`](./handoffs_spec-driven-devolopment.PVC/README.md) | Sempre que iniciar sessão: verificar gates G0-G9, métricas atuais, próximo PVC planejado |
| **Síntese Mercado × Arquitetura** | [`handoffs_spec-driven-devolopment.PVC/research/08-sintese-mercado-arquitetura-omni-os/README.md`](./handoffs_spec-driven-devolopment.PVC/research/08-sintese-mercado-arquitetura-omni-os/README.md) | Ao planejar novos PVCs ou decisões arquiteturais que envolvam padrões de mercado |
| **Metodologia PVC** | [`../metodologia_pvc_padroes_para_vibecoding_ia.md`](../metodologia_pvc_padroes_para_vibecoding_ia.md) | Para consultar padrões, templates, checklists por gate, fluxo operacional completo |
| **Plano Atual em Execução** | [`handoffs_spec-driven-devolopment.PVC/proximos planos/00-AVALIACAO_EXECUCAO_BANSHEE.md`](./handoffs_spec-driven-devolopment.PVC/proximos%20planos/00-AVALIACAO_EXECUCAO_BANSHEE.md) | Q4.1/Q4.2/Q4.3 comissionados (D-006/D-007/D-008 pagas); próximos candidatos em ordem: revocation list → adapter vetorial externo → endurecimento PAL. Histórico de planos anteriores: ver `handoffs_spec-driven-devolopment.PVC/proximos planos/` (documentação interna, não publicada) |

> **Regra PVC obrigatória:** Pequeno pode. Simulado pode temporariamente. Incompleto oculto não pode.
>
> **Processo mínimo por tarefa:**
> 1. Consulte o `README.md` do PVC para entender o estado atual e o próximo PVC planejado.
> 2. Consulte o plano `mantis-wolfsbane-venom.md` para confirmar prioridades.
> 3. Antes de implementar: verifique `handoffs_spec-driven-devolopment.PVC/MOCK_REGISTER.md` — não crie mock sem registrar.
> 4. Após implementar: atualize `handoffs_spec-driven-devolopment.PVC/COMMISSIONING_REPORT.md` com evidência.
> 5. Decisão arquitetural nova: crie ADR em `handoffs_spec-driven-devolopment.PVC/adr/ADRN-NNNN-titulo.md`.
> 6. Entrega: marque checklist em `handoffs_spec-driven-devolopment.PVC/DEFINITION_OF_DONE.md`.

## Novos Comandos CLI

| Comando | Descrição |
|---|---|
| `arreio run <spec> --model <nome> --permission-mode <modo> [--serve]` | Pipeline completo com optional gateway; persiste `security::permission_mode` |
| `arreio resume --model <nome> [--serve]` | Resume execução interrompida a partir do Blackboard |
| `arreio serve --port <porta>` | Inicia gateway HTTP com dashboard web + MCP + A2A |
| `arreio status` | Kanban ASCII + estado FSM |
| `arreio rollback` | Git reset --hard HEAD~1 |
| `arreio skills` | Lista skills do Tuple Space |
| `arreio mcp serve <transport>` | Inicia servidor MCP (stdio/http/sse) |
| `arreio a2a card` | Exibe o Agent Card JSON desta instância |
| `arreio vault add <prefix>` | Adiciona chave de API cifrada ao Vault |
| `arreio benchmark [filter]` | Executa suite comparativa via pipeline SYMBION |
| `arreio bridge claude` | MCP stdio bridge para Claude Code |
| `arreio bridge cursor --port <p>` | MCP SSE bridge para Cursor IDE |
| `arreio bridge hermes --port <p>` | API OpenAI-compatible para Hermes |
| `arreio bridge openclaw <url>` | Testa conexão com gateway OpenClaw |
| `arreio commission --src <dir> --test-output <arq> [--flows <json>] [--out <dir>]` | Self-Commissioning (PVC-Q3.3 via CLI): evidência real obrigatória, decisão calculada, artefatos `.generated`; `Reprovado` → exit ≠ 0 |
| `arreio credential issue --agent-id <id> --scope <s>... [--role r] [--ttl-hours h]` | Emite AgentCredential JWT (segredo via env `ARREIO_JWT_SECRET`, ≥32 chars; stdout = só o token) |
| `arreio credential verify <token>` | Verifica token e imprime claims (sub/role/scopes/exp/jti) |
| `arreio reason "<goal>" --mode direct\|cot\|tot\|react\|pal [--budget-*] [--session-id s]` | Raciocínio auditável standalone com budget e ledger hash-chain; ReAct usa executor read-only |
| `arreio reason ... --mode pal --execute-program --program-runner <cmd> [--program-ext e] [--program-timeout-sec n]` | Executa o programa PAL em sandbox via Hypervisor (PVC-Q4.3/ADR-0015): scan de conteúdo, arquivo em `.arreio/pal/`, 23 checks+blocklist+enforcer+timeout, resultado auditado no ledger. Sem as flags, o programa NÃO é executado |
| `arreio score set <node-id> [--urgency u] [--importance i] [--risk r] [--cost c] [--deadline epoch]` | Define score de priorização (tupla `dag::score:{id}`) |
| `arreio score list` | Lista nós com score composto |
| `arreio run\|resume ... [--agent-credential <jwt>] [--reasoning-mode <m>] [--prioritized]` | Zero-trust por invocação de tool; scaffold de raciocínio no Developer; despacho priorizado condicional (sem flags/scores, comportamento legado intocado) — PVC-Q4.1/ADR-0013 |
## Modos de permissao de tools

`--permission-mode` aceita: `default`, `plan`, `accept-edits`, `dont-ask`, `auto`, `auto-classifier`, `bypass`.

`arreio run` persiste o modo em `security::permission_mode`. `arreio resume` aceita `--permission-mode` opcional; se omitido, preserva o modo persistido.

Regras declarativas de tools sao carregadas de `/etc/arreio/rules`, `~/.arreio/rules`, `./.arreio/rules` e `./.arreio/local/rules`.

Formato por linha: `allow: read_file`, `ask: write_file(src/)`, `deny: exec`.

## Backend vetorial (PVC-Q4.2)

A busca de `bb.vector_query()` é plugável via trait `VectorBackend` (ADR-0014). Default: `linear` (comportamento original). Opt-in: `hnsw` (aproximado, determinístico, cache invalidado por revisão). Seleção: env `ARREIO_VECTOR_BACKEND=linear|hnsw` ou tupla `config::vector_backend`; valor desconhecido cai em linear com aviso. Adapter de storage externo (pgvector/Qdrant) é PVC futuro atrás do mesmo trait.
