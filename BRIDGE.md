# BRIDGE — Guia de Integração do O Arreio com Ecossistemas Externos

> **Versão:** 1.1.0 · **Data:** 2026-06-12
> **Idioma:** Português (texto) / Inglês (termos técnicos e protocolos)
>
> **Status (honesto):** os comandos e portas abaixo foram **verificados contra o binário** (`arreio --help`) e testados ponta a ponta em 2026-06-12. Onde algo é design pretendido e ainda não implementado, está marcado **🚧 roadmap**. Limitações conhecidas estão na seção 8.

---

## 1. Visão Geral

O **O Arreio** é projetado para ser um cidadão de primeira classe no ecossistema de agentes de IA. Este documento descreve as pontes (bridges) entre o Arreio e quatro plataformas:

1. **Claude Code / Claude Desktop** — via MCP stdio.
2. **Cursor** — via MCP SSE.
3. **Hermes Agent** — via API OpenAI-compatible.
4. **OpenClaw** — via cliente REST (teste de conexão; orquestração é 🚧 roadmap).

> **Comandos reais (resumo):** todos os bridges saem de `arreio bridge <ferramenta>`.
> ```bash
> arreio bridge claude              # MCP stdio  → Claude Code/Desktop
> arreio bridge cursor --port 7374  # MCP SSE     → Cursor
> arreio bridge hermes --port 7375  # OpenAI API  → Hermes e qualquer cliente OpenAI
> arreio bridge open-claw <url>     # testa conexão com um gateway OpenClaw
> ```
> O servidor MCP genérico (standalone) é `arreio mcp serve [stdio|http|sse] [--addr <host:porta>]`.

---

## 2. Claude Code / Claude Desktop

### 2.1 Arquitetura

O Claude Code suporta servidores MCP via transporte **stdio**, executando o servidor como subprocesso e trocando JSON-RPC 2.0 (framing `Content-Length`, estilo LSP) por stdin/stdout. O `stdout` é o canal do protocolo — o O Arreio emite todos os logs em `stderr`.

```
┌─────────────────┐         stdio (JSON-RPC, Content-Length)   ┌─────────────────────────┐
│  Claude Code    │  ◄──────────────────────────────────────►  │  arreio bridge claude   │
│  / Desktop      │                                            │  • Tools: 6 (ver §2.4)  │
└─────────────────┘                                            └─────────────────────────┘
```

### 2.2 Configuração

Arquivo de configuração MCP:
- Windows: `%APPDATA%\Claude\mcp.json` (Desktop) ou `.mcp.json` no projeto (Code)
- macOS: `~/Library/Application Support/Claude/mcp.json`
- Linux: `~/.config/Claude/mcp.json`

**Conteúdo (com o binário release no PATH):**
```json
{
  "mcpServers": {
    "arreio": {
      "command": "arreio",
      "args": ["bridge", "claude"],
      "env": { "RUST_LOG": "info" }
    }
  }
}
```

**Alternativa rodando do código-fonte (sem binário instalado):**
```json
{
  "mcpServers": {
    "arreio": {
      "command": "cargo",
      "args": ["run", "--quiet", "--bin", "arreio", "--", "bridge", "claude"],
      "cwd": "<CAMINHO/ABSOLUTO/PARA>/arreio",
      "env": {
        "RUST_LOG": "info",
        "PATH": "<HOME>/.cargo/bin;C:/msys64/ucrt64/bin"
      }
    }
  }
}
```

**Notas:**
- `cargo run` recompila na primeira invocação — para um handshake rápido prefira o binário release (`cargo build --release` e use `target/release/arreio`).
- Mantenha `RUST_LOG` apontando para `stderr` (padrão); nunca redirecione logs para `stdout`.

### 2.3 Verificação manual do handshake

Você pode confirmar o servidor sem o Claude, enviando dois frames JSON-RPC:
```bash
b1='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}'
b2='{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'
{ printf 'Content-Length: %d\r\n\r\n%s' "${#b1}" "$b1"
  printf 'Content-Length: %d\r\n\r\n%s' "${#b2}" "$b2"; } | arreio mcp serve stdio
```
A saída em `stdout` deve trazer o `initialize` (protocolVersion `2024-11-05`) e a lista de 6 tools com seus `input_schema`.

### 2.4 Tools expostas (reais)

| Tool | Argumentos (required) | Função |
|------|-----------------------|--------|
| `blackboard_read` | `cat`, `key` | Lê uma tupla do Blackboard |
| `blackboard_write` | `cat`, `key`, `value` | Escreve uma tupla |
| `create_task` | `spec` (JSON TaskSpec) | Cria um nó no DAG |
| `checkpoint_rollback` | `checkpoint_id` | Reverte para um checkpoint git |
| `safe_execute` | `cmd` | Executa um comando no sandbox do Hypervisor |
| `dag_status` | — | Status resumido do DAG |

Especificação detalhada de cada tool, resources (`blackboard://`, `dag://`, `fsm://`) e prompts em [`ARREIO-MCP.md`](ARREIO-MCP.md).

### 2.5 Troubleshooting

| Problema | Causa provável | Solução |
|----------|----------------|---------|
| `Failed to spawn MCP server` | binário não está no PATH | Use caminho absoluto em `command`, ou compile com `cargo build --release` |
| Cliente conecta mas não lista tools | versão antiga sem schemas | Atualize: as tools passaram a expor `input_schema` em 2026-06-12 |
| `Connection reset` (Windows) | AppControl bloqueou o binário recém-compilado | Rode o binário uma vez no terminal para o Windows escaneá-lo, depois reabra o cliente |
| Lixo no início do stream | logs no stdout (versão antiga) | Corrigido em 2026-06-12: logs vão para stderr |

---

## 3. Cursor

### 3.1 Arquitetura

O Cursor suporta MCP via **SSE**. O bridge do Arreio expõe `GET /sse` (abre o stream e anuncia o endpoint de POST) e `POST /message?session_id=<id>` (recebe JSON-RPC).

### 3.2 Configuração

**Passo 1 — Inicie o bridge Cursor:**
```bash
arreio bridge cursor --port 7374
```

**Passo 2 — No Cursor:** `Settings → MCP → Add New MCP Server`
- **Name:** `O Arreio`
- **Type / Transport:** `sse`
- **URL:** `http://localhost:7374/sse`

**Passo 3 — Teste:** no chat do Cursor, peça "use o Arreio para ver o status do DAG".

### 3.3 Troubleshooting

| Problema | Causa | Solução |
|----------|-------|---------|
| `Could not connect to SSE` | porta errada ou ocupada | Confirme `--port 7374` e a URL `/sse` |
| `No tools available` | handshake falhou | Veja o log do bridge em stderr |

> **Nota:** delegação para "Cursor Cloud" (`delegate()`) só ocorre se `CURSOR_CLOUD_ENDPOINT` estiver configurado; sem isso o bridge opera localmente e retorna um stub para essa rota específica (limitação registrada no MOCK_REGISTER, M-008).

---

## 4. Hermes Agent (e qualquer cliente OpenAI-compatible)

### 4.1 Arquitetura

O Arreio expõe um servidor HTTP que emula a API **OpenAI** (`/v1/models`, `/v1/chat/completions`), delegando ao `ProviderPool` (Ollama → OpenAI → Anthropic → Google → Azure, por prioridade).

### 4.2 Configuração

**Passo 1 — Inicie o bridge:**
```bash
arreio bridge hermes --port 7375
```

**Passo 2 — Aponte o cliente para o Arreio.** Exemplo de config do Hermes:
```yaml
llm:
  provider: openai
  base_url: "http://localhost:7375/v1"
  api_key: "no-key-required"   # ignorado no modo local
  model: "ollama"              # ou: openai | anthropic | google | azure
```

**Passo 3 — Teste:**
```bash
curl http://localhost:7375/v1/models
# {"object":"list","data":[{"id":"ollama",...},{"id":"openai",...}, ...]}
```

### 4.3 Modelos disponíveis (reais)

O `/v1/models` retorna um id por provider carregado no pool: `ollama`, `openai`, `anthropic`, `google`, `azure`. O provider é selecionado pelo campo `model` da requisição; chaves de cloud vêm das variáveis de ambiente (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, etc.) — sem a chave, aquele provider falha e o pool faz failover para o próximo.

> 🚧 **Roadmap:** "modelos de pipeline" (`arreio-orchestrator`, `arreio-architect`, …) que exporiam o pipeline de atores como um modelo único ainda não existem. Hoje cada `model` é um provider de inferência.

---

## 5. OpenClaw

### 5.1 O que existe hoje

```bash
arreio bridge open-claw <gateway_url>
```
Esse comando **testa a conexão** com um gateway OpenClaw (lista os cron jobs via REST) e reporta sucesso/falha. É o ponto de partida verificável da integração.

```bash
arreio bridge open-claw http://openclaw.local:8080
# [arreio] ✓ Conectado. Cron jobs: [...]   (ou ✗ Falha: <motivo>)
```

### 5.2 🚧 Roadmap (ainda não implementado)

Import/export/sincronização bidirecional de tarefas entre OpenClaw e o DAG do Arreio (`import`, `export`, `sync --daemon`) são **design pretendido**, não comandos atuais. O crate `arreio-bridge-openclaw` já traz o cliente REST base (`OpenClawClient`) sobre o qual essa orquestração será construída. Não copie exemplos de `import/export/sync` — eles não existem nesta versão.

---

## 6. Tabela comparativa

| Aspecto | Claude Code | Cursor | Hermes | OpenClaw |
|---------|-------------|--------|--------|----------|
| **Comando** | `bridge claude` | `bridge cursor --port 7374` | `bridge hermes --port 7375` | `bridge open-claw <url>` |
| **Protocolo** | MCP stdio | MCP SSE | OpenAI HTTP | REST |
| **Porta** | — (stdio) | 7374 | 7375 | — (cliente) |
| **Estado** | ✅ testado E2E | ✅ implementado | ✅ testado E2E | ⚠️ só teste de conexão |

---

## 7. Rodando vários serviços juntos

`arreio run <spec> --serve` (ou `arreio resume --serve`) sobe, em threads de background, o conjunto de serviços a partir de uma porta base (default 7373):

| Serviço | Porta | Observação |
|---------|-------|------------|
| Gateway HTTP / dashboard | `base` (7373) | também via `arreio serve --port 7373` (só o gateway) |
| MCP server (HTTP) | `base+1` (7374) | |
| A2A server | `base+2` (7375) | ver [`ARREIO-A2A.md`](ARREIO-A2A.md) |

Os bridges da seção 2–5 são processos **separados**, iniciados sob demanda — não fazem parte desse conjunto de background.

---

## 8. Limitações conhecidas (registradas, não escondidas)

- **OpenClaw**: só `teste de conexão`; orquestração import/export/sync é roadmap (§5.2).
- **Cursor Cloud delegate**: stub sem `CURSOR_CLOUD_ENDPOINT` (MOCK_REGISTER M-008).
- **MCP/A2A servers**: cobertos por testes unitários e smoke E2E; faltam testes E2E de threads de longa duração (dívida D-005).
- **A2A AgentCard**: o campo `url` pode trazer um placeholder; o endereço real é o do log `[a2a] Iniciando em http://...`.

---

## 9. Considerações de segurança

1. Não exponha o Arreio diretamente à internet sem reverse proxy + TLS.
2. stdio é para uso local (subprocesso); para rede use HTTP/SSE em `127.0.0.1` ou atrás de proxy autenticado.
3. Monitore o audit trail (`~/.arreio/audit/`) para chamadas de bridges externas.

> **Nota:** novas plataformas podem ser adicionadas seguindo os padrões aqui. Toda nova bridge deve ter: (1) documentação neste arquivo, (2) testes no respectivo crate `arreio-bridge-*`, e (3) entrada no `AGENTS.md`.
