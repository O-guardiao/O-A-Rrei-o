# BRIDGE — Guia de Integração do Arreio com Ecossistemas Externos

> **Versão:** 1.0.0  
> **Status:** Draft  
> **Autor:** O Arreio Integration Team  
> **Data:** 2026-05-16  
> **Idioma:** Português (comentários/exemplos) / Inglês (termos técnicos e protocolos)

---

## 1. Visão Geral

O **O Arreio** é projetado para ser um cidadão de primeira classe no ecossistema de agentes de IA. Este documento descreve os pontes de integração (bridges) entre o Arreio e quatro plataformas principais:

1. **Claude Code** — via MCP stdio + wrapper headless.
2. **Cursor** — via MCP SSE + sandbox de segurança.
3. **Hermes Agent** — via API server OpenAI-compatible.
4. **OpenClaw** — via REST client + import/export de tarefas.

Cada bridge é documentada com:
- Arquitetura de conexão.
- Passos de configuração.
- Exemplos de interação.
- Troubleshooting comum.

---

## 2. Claude Code

### 2.1 Arquitetura

O Claude Code suporta servidores MCP via transporte **stdio**, executando o servidor como um subprocesso e se comunicando via JSON-RPC 2.0 sobre stdin/stdout.

```
┌─────────────────┐         stdio (JSON-RPC)          ┌─────────────────────────┐
│  Claude Code    │  ◄──────────────────────────────►  │  O Arreio MCP Server     │
│  (Electron app) │                                    │  (cargo run --bin arreio) │
│                 │                                    │  • Transport: stdio     │
│                 │                                    │  • Tools: create_task   │
│                 │                                    │  • Resources: blackboard│
│                 │                                    │  • Prompts: planning    │
└─────────────────┘                                    └─────────────────────────┘
```

### 2.2 Configuração

Crie ou edite o arquivo de configuração MCP do Claude Code:

**Caminho:**
- Windows: `%APPDATA%\Claude\mcp.json`
- macOS: `~/Library/Application Support/Claude/mcp.json`
- Linux: `~/.config/Claude/mcp.json`

**Conteúdo:**
```json
{
  "mcpServers": {
    "arreio": {
      "command": "cargo",
      "args": [
        "run",
        "--bin", "arreio",
        "--",
        "mcp",
        "--transport", "stdio"
      ],
      "cwd": "<CAMINHO/ABSOLUTO/PARA>/arreio",
      "env": {
        "RUST_LOG": "info",
        "PATH": "<HOME>/.cargo/bin;C:/msys64/ucrt64/bin"
      },
      "disabled": false,
      "autoApprove": ["blackboard_read", "dag_status"]
    }
  }
}
```

**Notas de configuração:**
- `cwd` deve apontar para o diretório raiz do workspace `arreio/`.
- `PATH` deve incluir tanto o Cargo quanto o MSYS2 UCRT64 (para o linker).
- `autoApprove` permite que o Claude execute tools de leitura sem confirmação do usuário. **Nunca** inclua `safe_execute` ou `checkpoint_rollback` em `autoApprove`.

### 2.3 Wrapper Headless (Modo Autônomo)

Para execução autônoma sem a UI do Claude Code, use o wrapper headless:

```bash
# arreio/scripts/claude-bridge.sh (ou .ps1 no Windows)
#!/bin/bash
# Wrapper headless para Claude Code + O Arreio

set -e

export ARREIO_MCP_TRANSPORT=stdio
export ARREIO_BLACKBOARD_PATH="C:/dev/arreio-blackboard"

# Inicia o Arreio em background
arreio mcp --transport stdio &
ARREIO_PID=$!

# Aguarda handshake
sleep 2

# Executa o Claude Code em modo headless
claude --mcp-server arreio --prompt "Crie uma tarefa para adicionar logging estruturado ao arreio-kernel"

# Cleanup
trap "kill $ARREIO_PID" EXIT
```

### 2.4 Exemplo de Sessão

**Usuário no Claude Code:**
```
/claude use arreio
Preciso que você refatore o módulo de memória para usar um GraphStore em vez de HashMap.
```

**Claude (via MCP):**
```
Vou delegar esta tarefa ao Arreio.

[Chamando tool: create_task]
  spec: "Refatorar arreio-memory para usar GraphStore ao invés de HashMap"
  actor: "auto"

✓ Tarefa criada: task-05e2
  Estado FSM: Idle → Exploration

[Chamando tool: dag_status]

DAG Status:
  • 1 nó pendente (task-05e2)
  • 0 em execução
  • 0 concluídos

[Aguardando execução...]

[Chamando tool: blackboard_read]
  pattern: ["task", "task-05e2", null]

✓ Task concluída. Artefatos gerados:
  - crates/arreio-memory/src/graph_store.rs (142 linhas)
  - crates/arreio-memory/src/tests/graph_store_tests.rs (68 linhas)

[Chamando tool: safe_execute]
  command: "cargo test -p arreio-memory"

✓ Todos os testes passaram (exit code 0).
```

### 2.5 Troubleshooting

| Problema | Causa Provável | Solução |
|----------|----------------|---------|
| `Failed to spawn MCP server` | PATH não inclui cargo ou gcc | Adicione `%USERPROFILE%/.cargo/bin` e `C:/msys64/ucrt64/bin` ao env |
| `Connection reset` | Windows AppControl bloqueou o binário | Execute `cargo test -p arreio-cli` manualmente para "aquecer" o AppControl |
| `Tool not found` | Configuração JSON malformada | Valide o JSON em https://jsonlint.com/ |
| `Timeout ao criar task` | Ollama não está rodando | Inicie o Ollama: `ollama serve` |
| `Exit code -2` | Comando excedeu timeout | Aumente `timeout_seconds` no argumento da tool |

---

## 3. Cursor

### 3.1 Arquitetura

O Cursor IDE suporta servidores MCP via transporte **SSE** (Server-Sent Events) ou **HTTP**. A configuração é feita na interface de settings do Cursor.

```
┌─────────────────┐         SSE/HTTP                    ┌─────────────────────────┐
│  Cursor IDE     │  ◄──────────────────────────────►  │  O Arreio MCP Server     │
│  (VS Code fork) │     JSON-RPC + event stream        │  (cargo run --bin arreio) │
│                 │                                    │  • Transport: sse       │
│                 │                                    │  • Port: 7373           │
│                 │                                    │  • Sandbox: ativo       │
└─────────────────┘                                    └─────────────────────────┘
```

### 3.2 Configuração

**Passo 1 — Inicie o Arreio em modo SSE:**
```bash
cd arreio
$env:PATH = "$env:USERPROFILE\.cargo\bin;C:\msys64\ucrt64\bin;$env:PATH"
cargo run --bin arreio -- mcp --transport sse --port 7373
```

**Passo 2 — Configure no Cursor:**
1. Abra o Cursor.
2. Vá em `Settings` → `Features` → `MCP Servers`.
3. Clique em `Add New MCP Server`.
4. Preencha:
   - **Name:** `O Arreio`
   - **Transport:** `sse`
   - **URL:** `http://localhost:7373/mcp/sse`

**Passo 3 — Teste a conexão:**
No chat do Cursor, diga:
```
Use o Arreio para verificar o status do DAG atual.
```

### 3.3 Sandbox de Segurança no Cursor

O Cursor executa tools MCP em um sandbox interno. Para garantir compatibilidade:

1. **Não use paths absolutos sensíveis** nas descriptions das tools (ex: não mencione `C:/Users/<usuario>/Documents`).
2. **Prefira resources sobre argumentos de path** — o cliente lê o resource e injeta o conteúdo no contexto.
3. **Habilite o McpSandbox** no Arreio para validar descriptions antes do handshake.

### 3.4 Exemplo de Sessão

**Usuário no Cursor:**
```
@O Arreio Crie um checkpoint do estado atual e depois adicione uma nova skill de parsing de logs.
```

**Cursor (via MCP):**
```
[Chamando tool: checkpoint_rollback com steps=0 (apenas checkpoint)]
✓ Checkpoint criado. Commit: a1b2c3d

[Chamando tool: create_task]
  spec: "Adicionar skill de parsing de logs ao arreio-skills"
  actor: "developer"

✓ Tarefa criada: task-06f3

[Chamando tool: dag_status]
  total_nodes: 3
  pending: 2
  running: 1

[Streaming via SSE...]
  event: state → working
  event: artifact → skill_log_parser.rs
  event: state → completed

✓ Skill adicionada com sucesso. Testes: 12 passaram, 0 falharam.
```

### 3.5 Troubleshooting

| Problema | Causa Provável | Solução |
|----------|----------------|---------|
| `Could not connect to SSE` | Firewall bloqueando porta 7373 | Abra a porta 7373 para localhost |
| `No tools available` | Handshake falhou | Verifique se o Arreio logou `MCP initialize ok` |
| `SSE connection dropped` | Timeout de inatividade | O Arreio envia ping a cada 30s; verifique `RUST_LOG` |
| `Tool execution failed` | Windows AppControl | Execute o binário manualmente uma vez para liberar |

---

## 4. Hermes Agent

### 4.1 Arquitetura

O **Hermes Agent** é uma plataforma de agentes que se comunica via API **OpenAI-compatible**. O Arreio expõe um servidor HTTP que emula a API de Chat Completions, permitindo que o Hermes trate o Arreio como um modelo LLM.

```
┌─────────────────┐      OpenAI-compatible API       ┌─────────────────────────┐
│  Hermes Agent   │  ◄────────────────────────────►  │  O Arreio OpenAI Bridge  │
│  (Python/Node)  │     POST /v1/chat/completions    │  (arreio-gateway)         │
│                 │     Bearer Token auth            │  • Models: arreio-*    │
│                 │     Streaming: application/json  │  • Tools expostas como  │
│                 │                                  │    function calls       │
└─────────────────┘                                  └─────────────────────────┘
```

### 4.2 Configuração

**Passo 1 — Inicie o Arreio em modo OpenAI bridge:**
```bash
cd arreio
$env:PATH = "$env:USERPROFILE\.cargo\bin;C:\msys64\ucrt64\bin;$env:PATH"
cargo run --bin arreio -- serve --port 7373 --bridge openai
```

**Passo 2 — Configure o Hermes para apontar para o Arreio:**

No arquivo de configuração do Hermes (`hermes_config.yaml`):
```yaml
llm:
  provider: openai
  base_url: "http://localhost:7373/v1"
  api_key: "arreio-no-key-required"  # O Arreio ignora a API key em modo local
  model: "arreio-orchestrator"
  temperature: 0.2
  max_tokens: 4096

tools:
  - name: "create_task"
    endpoint: "http://localhost:7373/v1/tools/create_task"
  - name: "safe_execute"
    endpoint: "http://localhost:7373/v1/tools/safe_execute"
```

### 4.3 Mapeamento de Function Calls

O Arreio OpenAI Bridge mapeia as tools MCP para o formato `function` da API OpenAI:

**Request do Hermes:**
```json
{
  "model": "arreio-orchestrator",
  "messages": [
    { "role": "system", "content": "Você é o Arreio." },
    { "role": "user", "content": "Crie uma tarefa para otimizar o parser do arreio-ast" }
  ],
  "tools": [
    {
      "type": "function",
      "function": {
        "name": "create_task",
        "description": "Cria uma nova tarefa no Blackboard e insere um nó no DAG.",
        "parameters": {
          "type": "object",
          "properties": {
            "spec": { "type": "string" },
            "priority": { "type": "integer" },
            "actor": { "type": "string", "enum": ["architect", "developer", "inspector", "auto"] }
          },
          "required": ["spec"]
        }
      }
    }
  ],
  "tool_choice": "auto"
}
```

**Response do Arreio (function call):**
```json
{
  "id": "chatcmpl-arreio-123",
  "object": "chat.completion",
  "created": 1680000000,
  "model": "arreio-orchestrator",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": null,
        "tool_calls": [
          {
            "id": "call_abc123",
            "type": "function",
            "function": {
              "name": "create_task",
              "arguments": "{\"spec\":\"Otimizar o parser do arreio-ast para reduzir alocações\",\"priority\":3,\"actor\":\"developer\"}"
            }
          }
        ]
      },
      "finish_reason": "tool_calls"
    }
  ]
}
```

### 4.4 Exemplo de Sessão

**Hermes (código Python):**
```python
from hermes import Agent

agent = Agent(
    llm_base_url="http://localhost:7373/v1",
    model="arreio-orchestrator"
)

response = agent.run(
    "Crie uma tarefa para adicionar suporte a Python no arreio-ast e execute os testes."
)

print(response)
# Saída:
# Task criada: task-07g4
# DAG: 3 nós planejados
# Testes: cargo test -p arreio-ast → 0 falhas
```

### 4.5 Modelos Disponíveis

| Model ID | Descrição |
|----------|-----------|
| `arreio-orchestrator` | Pipeline completo (Arquiteto → DAG → Desenvolvedor → Inspetor) |
| `arreio-architect` | Apenas o ator Arquiteto (planning e decomposição) |
| `arreio-developer` | Apenas o ator Desenvolvedor (code generation) |
| `arreio-inspector` | Apenas o ator Inspetor (code review e security audit) |
| `arreio-hypervisor` | Apenas execução segura de comandos |

---

## 5. OpenClaw

### 5.1 Arquitetura

O **OpenClaw** é uma plataforma de orquestração de agentes com REST API. O Arreio integra-se via cliente REST que consome a API do OpenClaw e exporta/importa tarefas entre os dois sistemas.

```
┌─────────────────┐         REST JSON                ┌─────────────────────────┐
│  OpenClaw       │  ◄────────────────────────────►  │  O Arreio OpenClaw       │
│  (API Server)   │     POST /api/v1/tasks           │  Bridge (arreio-cli)      │
│                 │     GET  /api/v1/tasks/{id}      │  • Import: tarefas OC   │
│                 │     POST /api/v1/artifacts       │    → O Arreio DAG        │
│                 │                                  │  • Export: resultados   │
│                 │                                  │    → OpenClaw artifacts │
└─────────────────┘                                  └─────────────────────────┘
```

### 5.2 Configuração

No arquivo `configs/openclaw.toml`:
```toml
[openclaw]
enabled = true
base_url = "http://openclaw.local:8080/api/v1"
api_key = "oc_api_key_a1b2c3d4"
poll_interval_seconds = 5
sync_direction = "bidirectional"  # "import", "export", "bidirectional"

[mapping]
# Mapeia status do OpenClaw para estados da FSM O Arreio
openclaw_status.queued = "Idle"
openclaw_status.running = "Execution"
openclaw_status.completed = "Consolidation"
openclaw_status.failed = "StrategicRetreat"

# Mapeia tipos de tarefa OpenClaw para skills O Arreio
openclaw_type.code = "code-generation"
openclaw_type.shell = "safe-execution"
openclaw_type.plan = "dag-orchestration"
```

### 5.3 Import de Tarefas (OpenClaw → O Arreio)

```bash
# Importa tarefas pendentes do OpenClaw para o Arreio
cargo run --bin arreio -- bridge openclaw import --status queued --limit 10

# Fluxo:
# 1. Consulta GET /api/v1/tasks?status=queued&limit=10
# 2. Para cada tarefa, cria uma tupla no Blackboard.
# 3. Insere nó no DAG com skill mapeada.
# 4. Atualiza status no OpenClaw para "running".
```

### 5.4 Export de Resultados (O Arreio → OpenClaw)

```bash
# Exporta artefatos de tarefas concluídas para o OpenClaw
cargo run --bin arreio -- bridge openclaw export --task task-08h5

# Fluxo:
# 1. Lê artefatos do Blackboard para task-08h5.
# 2. POST /api/v1/artifacts com o conteúdo do arquivo.
# 3. Atualiza a tarefa no OpenClaw com links para os artefatos.
```

### 5.5 Sincronização Bidirecional

```bash
# Inicia loop de sincronização contínua
cargo run --bin arreio -- bridge openclaw sync --daemon

# Comportamento:
# - A cada 5 segundos, verifica novas tarefas no OpenClaw.
# - Importa para o Arreio e inicia execução.
# - Quando concluída, exporta resultados de volta.
# - Loga todas as operações no Audit Trail.
```

### 5.6 Exemplo de Sessão

**Cenário:** Um workflow no OpenClaw cria uma tarefa de refactoring. O Arreio importa, executa e devolve o resultado.

**OpenClaw (criação da tarefa):**
```bash
curl -X POST http://openclaw.local:8080/api/v1/tasks \
  -H "Authorization: Bearer oc_api_key_a1b2c3d4" \
  -H "Content-Type: application/json" \
  -d '{
    "title": "Refatorar parser do arreio-ast",
    "description": "Substituir regex fallback por parser nom parser_combinator",
    "type": "code",
    "priority": "high"
  }'
```

**O Arreio (importação e execução):**
```bash
# O daemon de sincronização detecta a nova tarefa automaticamente
# e a importa para o DAG.

# Verifique o status:
cargo run --bin arreio -- status

# Saída:
# ┌─────────┬─────────────┬────────────┬──────────┐
# │ Task ID │ Estado FSM  │ DAG Status │ Progress │
# ├─────────┼─────────────┼────────────┼──────────┤
# │ task-09i│ Execution   │ 2/4 nós    │ 50%      │
# └─────────┴─────────────┴────────────┴──────────┘

# Após conclusão, o OpenClaw recebe os artefatos:
curl http://openclaw.local:8080/api/v1/tasks/oc-task-123/artifacts
# → ["parser_combinator.rs", "tests_parser.rs", "benchmark.json"]
```

### 5.7 Troubleshooting

| Problema | Causa Provável | Solução |
|----------|----------------|---------|
| `Connection refused` | OpenClaw não está acessível | Verifique `base_url` e conectividade de rede |
| `401 Unauthorized` | API key inválida | Regenere a chave no painel do OpenClaw |
| `Mapping error` | Tipo de tarefa não mapeado | Adicione o tipo em `configs/openclaw.toml` |
| `Sync loop travado` | Task em estado inconsistente | Pare o daemon, execute `arreio rollback`, reinicie |

---

## 6. Tabela Comparativa de Bridges

| Aspecto | Claude Code | Cursor | Hermes | OpenClaw |
|---------|-------------|--------|--------|----------|
| **Protocolo** | MCP stdio | MCP SSE | OpenAI API | REST JSON |
| **Transporte** | Pipe local | HTTP/SSE | HTTP | HTTP |
| **Autenticação** | Nenhuma (local) | Nenhuma (local) | Bearer Token | API Key |
| **Streaming** | Não | Sim (SSE) | Sim (SSE/chunked) | Não (polling) |
| **Direção** | O Arreio como server | O Arreio como server | O Arreio como server | Bidirecional |
| **Uso Ideal** | Desenvolvimento local | Desenvolvimento IDE | Integração programática | Orquestração cross-platform |
| **Sandbox** | Dependente do cliente | Cursor sandbox + O Arreio | O Arreio Hypervisor | O Arreio Hypervisor |
| **Complexidade** | Baixa | Baixa | Média | Média |

---

## 7. Scripts de Automatização

### 7.1 Script de Inicialização Multi-Bridge

```powershell
# arreio/scripts/start-bridges.ps1
# Inicia o Arreio com múltiplas bridges simultâneas

$env:PATH = "$env:USERPROFILE\.cargo\bin;C:\msys64\ucrt64\bin;$env:PATH"
$env:RUST_LOG = "info"

# Inicia o gateway com MCP SSE + OpenAI Bridge
Start-Process -FilePath "cargo" -ArgumentList @(
    "run", "--bin", "arreio", "--",
    "serve", "--port", "7373",
    "--mcp-sse", "--openai-bridge"
) -NoNewWindow -WorkingDirectory "<CAMINHO/ABSOLUTO/PARA>/arreio"

Write-Host "O Arreio Gateway iniciado em http://localhost:7373"
Write-Host "  MCP SSE:     http://localhost:7373/mcp/sse"
Write-Host "  OpenAI API:  http://localhost:7373/v1"
Write-Host "  A2A:         http://localhost:7373/a2a"
```

### 7.2 Script de Teste de Conectividade

```bash
#!/bin/bash
# arreio/scripts/test-bridges.sh

set -e

ARREIO_URL="http://localhost:7373"

echo "=== Testando bridges do Arreio ==="

echo "[1/4] MCP SSE..."
curl -s -o /dev/null -w "%{http_code}" "$ARREIO_URL/mcp/sse" | grep -q "200" && echo "OK" || echo "FALHOU"

echo "[2/4] A2A AgentCard..."
curl -s "$ARREIO_URL/a2a/agent-card" | jq -r '.name' | grep -q "O Arreio" && echo "OK" || echo "FALHOU"

echo "[3/4] OpenAI Bridge (models)..."
curl -s "$ARREIO_URL/v1/models" | jq -r '.data[0].id' | grep -q "arreio" && echo "OK" || echo "FALHOU"

echo "[4/4] Health Check..."
curl -s "$ARREIO_URL/health" | jq -r '.status' | grep -q "ok" && echo "OK" || echo "FALHOU"

echo "=== Teste concluído ==="
```

---

## 8. Considerações de Segurança nas Integrações

1. **Nunca exponha o Arreio diretamente à internet** sem reverse proxy + mTLS.
2. **Desative stdio em produção** — use apenas HTTP/SSE com autenticação.
3. **Revogue API keys periodicamente** — especialmente para Hermes e OpenClaw.
4. **Monitore o Audit Trail** para detectar chamadas suspeitas de bridges externas.
5. **Use o McpSandbox** para validar descriptions antes de expor tools a clientes MCP.

---

## 9. Glossário

| Termo (EN) | Definição (PT) |
|------------|----------------|
| Bridge | Ponto de integração entre O Arreio e uma plataforma externa |
| stdio | Standard input/output — transporte via pipe de subprocesso |
| SSE | Server-Sent Events — stream HTTP unidirecional do servidor |
| MCP | Model Context Protocol — protocolo da Anthropic para LLM ↔ tools |
| OpenAI-compatible API | API que emula o formato de requests/responses da OpenAI |
| Function Call | Mecanismo da OpenAI API para invocar funções externas |
| Headless | Execução sem interface gráfica, via CLI ou script |
| Bidirectional Sync | Sincronização de dados em ambas as direções |
| Artifact | Arquivo ou dado produzido pela execução de uma tarefa |
| Bearer Token | Token de autenticação enviado no header HTTP |

---

> **Nota final:** As bridges são pontes vivas entre o Arreio e o ecossistema. Novas plataformas (Windsurf, Continue, Copilot, etc.) podem ser adicionadas seguindo os padrões aqui estabelecidos. Toda nova bridge deve ter: (1) documentação neste arquivo, (2) testes unitários no crate `arreio-gateway`, e (3) entrada no `AGENTS.md` raiz.
