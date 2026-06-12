# ARREIO-A2A — Especificação de Compatibilidade A2A (Agent-to-Agent)

> **Versão:** 1.0.0  
> **Status:** Draft  
> **Autor:** O Arreio Core Team  
> **Data:** 2026-05-16  
> **Idioma:** Português (comentários/exemplos) / Inglês (termos técnicos e protocolos)

---

## 1. Visão Geral

O **O Arreio** implementa compatibilidade completa com o protocolo **A2A (Agent-to-Agent)**, permitindo que o sistema opere tanto como **agente produtor** (capaz de receber e executar tarefas delegadas por outros agentes) quanto como **agente consumidor** (capaz de delegar subtarefas para agentes externos compatíveis com A2A).

O protocolo A2A é baseado em HTTP/JSON e define um modelo de comunicação stateless entre agentes autônomos, onde:

- Cada agente publica um **AgentCard** que descreve suas capacidades.
- Tarefas são criadas via POST e acompanhadas via GET polling ou SSE streaming.
- O ciclo de vida de uma task segue estados bem definidos, permitindo interoperabilidade entre diferentes implementações.

O Arreio integra o A2A Layer no crate `arreio-gateway`, reaproveitando o servidor HTTP síncrono já existente. Não há adição de runtime async — o polling e o streaming são implementados com mecanismos síncronos de buffer circular e chunked transfer encoding.

---

## 2. Arquitetura A2A no Arreio

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                            O Arreio A2A Layer                                │
│  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────────────────┐  │
│  │  A2A Server     │  │  A2A Client     │  │  Task Lifecycle Manager     │  │
│  │  (arreio-gateway) │  │  (arreio-actors)  │  │  (integrado com FSM + DAG)  │  │
│  │                 │  │                 │  │                             │  │
│  │  • AgentCard    │  │  • Descoberta   │  │  • submitted → working      │  │
│  │  • Endpoints    │  │  • Delegação    │  │  • input-required → done    │  │
│  │  • SSE stream   │  │  • Result poll  │  │  • failed → retry/rollback  │  │
│  └────────┬────────┘  └────────┬────────┘  └─────────────┬───────────────┘  │
│           │                    │                         │                  │
│           └────────────────────┴─────────────────────────┘                  │
│                              │                                              │
│                         ┌────▼────┐                                        │
│                         │Blackboard│ (estado central + Tuple Space)        │
│                         │(arreio-   │                                        │
│                         │ kernel) │                                        │
│                         └─────────┘                                        │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 3. AgentCard

O **AgentCard** é o documento de identidade do Arreio no ecossistema A2A. Ele é servido em `GET /a2a/agent-card` e segue o schema JSON definido pelo protocolo A2A.

### 3.1 Estrutura do AgentCard

```json
{
  "name": "O Arreio",
  "description": "Sistema Operacional para LLMs com orquestração de atores via Blackboard, FSM e DAG. Executa tarefas de software engineering com sandbox de segurança.",
  "url": "http://localhost:7373/a2a",
  "version": "1.0.0",
  "capabilities": {
    "streaming": true,
    "pushNotifications": false,
    "stateTransitionCallback": true
  },
  "authentication": {
    "schemes": ["none"],
    "credentials": null
  },
  "defaultInputModes": ["text"],
  "defaultOutputModes": ["text", "file"],
  "skills": [
    {
      "id": "code-generation",
      "name": "Geração de Código",
      "description": "Gera código Rust (ou outras linguagens suportadas) a partir de especificações, com revisão de segurança.",
      "tags": ["rust", "code-generation", "safety"],
      "examples": [
        "Crie um módulo de parsing JSON com serde",
        "Refatore esta função para eliminar unwraps"
      ],
      "inputModes": ["text"],
      "outputModes": ["text", "file"]
    },
    {
      "id": "safe-execution",
      "name": "Execução Segura",
      "description": "Executa comandos de shell em sandbox com blocklist, timeout e watchdog.",
      "tags": ["shell", "sandbox", "security"],
      "examples": [
        "Execute cargo test no crate arreio-kernel",
        "Compile o workspace e reporte erros"
      ],
      "inputModes": ["text"],
      "outputModes": ["text"]
    },
    {
      "id": "dag-orchestration",
      "name": "Orquestração DAG",
      "description": "Decompõe tarefas complexas em grafos acíclicos e executa scheduling topológico.",
      "tags": ["dag", "planning", "orchestration"],
      "examples": [
        "Planeje a implementação de uma nova feature em 5 subtarefas",
        "Qual é o critical path do DAG atual?"
      ],
      "inputModes": ["text"],
      "outputModes": ["text"]
    },
    {
      "id": "checkpoint-rollback",
      "name": "Checkpoint e Rollback",
      "description": "Cria checkpoints via git e realiza rollback automático em caso de falha.",
      "tags": ["git", "rollback", "safety"],
      "examples": [
        "Crie um checkpoint antes da próxima tarefa",
        "Reverta o último commit e restaure o estado"
      ],
      "inputModes": ["text"],
      "outputModes": ["text"]
    }
  ],
  "limits": {
    "maxTokensPerTask": 32768,
    "maxTasksPerMinute": 10,
    "maxConcurrentTasks": 3,
    "timeoutSeconds": 300
  }
}
```

### 3.2 Capacidades Anunciadas

| Capacidade | Valor | Descrição |
|------------|-------|-----------|
| `streaming` | `true` | Suporta streaming de updates via SSE durante execução |
| `pushNotifications` | `false` | Não suporta callbacks webhook (ambiente local) |
| `stateTransitionCallback` | `true` | Notifica o cliente sobre transições de estado da task |

---

## 4. Ciclo de Vida da Task (Task Lifecycle)

O Arreio implementa o ciclo de vida completo de tasks A2A, mapeando os estados internos da FSM para os estados do protocolo.

### 4.1 Estados

```
submitted
    │
    ▼
working ───────────────────┐
    │                      │
    ▼                      │
input-required ◄───────────┤ (loop de interação)
    │                      │
    ▼                      │
completed ◄────────────────┘
    │
    ▼
failed
```

| Estado | Descrição | Mapeamento FSM Interno |
|--------|-----------|------------------------|
| `submitted` | Task recebida, ainda não processada | `Idle` |
| `working` | Task em execução ativa | `Exploration`, `Planning`, `Execution`, `Evaluation`, `Correction` |
| `input-required` | Aguardando informação adicional do agente cliente | `Evaluation` (quando o Inspetor rejeita e solicita correção) |
| `completed` | Task finalizada com sucesso | `Consolidation` |
| `failed` | Task falhou ou foi bloqueada | `StrategicRetreat` |

### 4.2 Transições de Estado

Toda transição de estado gera um evento que é:
1. Persistido no Blackboard como tupla `("a2a_task", task_id, "state", new_state)`.
2. Enviado ao cliente via SSE (se conectado).
3. Logado no audit trail com timestamp e hash encadeada.

**Diagrama de transições detalhado:**
```
submitted → working:    task é inserida no DAG e a FSM transiciona para Exploration
working → input-required:   ator precisa de esclarecimento (ex: spec ambígua)
input-required → working:   cliente fornece a informação solicitada
working → completed:    todos os nós do DAG concluídos com sucesso
working → failed:       watchdog detectou loop, ou Hypervisor bloqueou comando crítico
input-required → failed:    cliente não respondeu dentro do timeout (300s)
```

---

## 5. Endpoints HTTP

Todos os endpoints abaixo são expostos pelo `arreio-gateway` sob o prefixo `/a2a`.

### 5.1 `GET /a2a/agent-card`

Retorna o AgentCard do Arreio.

**Request:**
```http
GET /a2a/agent-card HTTP/1.1
Host: localhost:7373
Accept: application/json
```

**Response:**
```http
HTTP/1.1 200 OK
Content-Type: application/json
Content-Length: 1842

{ ... AgentCard JSON ... }
```

---

### 5.2 `POST /a2a/tasks`

Cria uma nova task no Arreio.

**Request:**
```http
POST /a2a/tasks HTTP/1.1
Host: localhost:7373
Content-Type: application/json

{
  "id": "a2a-task-99x1",
  "sessionId": "session-abc-123",
  "message": {
    "role": "user",
    "parts": [
      {
        "type": "text",
        "text": "Crie um módulo de autenticação JWT para o arreio-gateway com validação de tokens RS256"
      }
    ]
  },
  "skill": "code-generation",
  "metadata": {
    "priority": 4,
    "requesting_agent": "claude-desktop-4.0"
  }
}
```

**Response (201 Created):**
```http
HTTP/1.1 201 Created
Content-Type: application/json
Location: /a2a/tasks/a2a-task-99x1

{
  "id": "a2a-task-99x1",
  "sessionId": "session-abc-123",
  "status": {
    "state": "submitted",
    "message": "Task recebida e enfileirada no DAG"
  },
  "createdAt": "2026-05-16T10:05:00Z",
  "updatedAt": "2026-05-16T10:05:00Z"
}
```

**Fluxo interno:**
1. Parseia o JSON do corpo da requisição.
2. Gera `task_id` se não fornecido (ou usa o fornecido).
3. Valida se a skill solicitada existe no AgentCard.
4. Escreve a tupla `("a2a_task", task_id, spec, metadata)` no Blackboard.
5. Insere nó inicial no DAG com estado `pending`.
6. Transiciona FSM de `Idle` para `Exploration`.
7. Retorna 201 com o estado `submitted`.

---

### 5.3 `GET /a2a/tasks/{id}`

Consulta o estado e o histórico de uma task específica.

**Request:**
```http
GET /a2a/tasks/a2a-task-99x1 HTTP/1.1
Host: localhost:7373
Accept: application/json
```

**Response (em execução):**
```http
HTTP/1.1 200 OK
Content-Type: application/json

{
  "id": "a2a-task-99x1",
  "sessionId": "session-abc-123",
  "status": {
    "state": "working",
    "message": "Desenvolvedor executando: gerando código do módulo JWT"
  },
  "artifacts": [],
  "history": [
    {
      "role": "user",
      "parts": [
        { "type": "text", "text": "Crie um módulo de autenticação JWT..." }
      ]
    },
    {
      "role": "agent",
      "parts": [
        { "type": "text", "text": "Arquiteto decompôs em 3 subtarefas: schema, implementação, testes." }
      ]
    }
  ],
  "createdAt": "2026-05-16T10:05:00Z",
  "updatedAt": "2026-05-16T10:06:30Z"
}
```

**Response (concluída):**
```http
HTTP/1.1 200 OK
Content-Type: application/json

{
  "id": "a2a-task-99x1",
  "sessionId": "session-abc-123",
  "status": {
    "state": "completed",
    "message": "Task concluída. 3 subtarefas executadas, 0 falhas."
  },
  "artifacts": [
    {
      "name": "jwt_auth.rs",
      "type": "file",
      "mimeType": "text/rust",
      "content": "use jsonwebtoken::{decode, Validation, Algorithm};\n// ... código completo ...",
      "metadata": {
        "lines": 142,
        "tests_passed": 8,
        "inspector_approved": true
      }
    },
    {
      "name": "test_jwt.rs",
      "type": "file",
      "mimeType": "text/rust",
      "content": "// ... tests ..."
    }
  ],
  "history": [ ... ],
  "createdAt": "2026-05-16T10:05:00Z",
  "updatedAt": "2026-05-16T10:12:45Z"
}
```

---

### 5.4 `POST /a2a/tasks/{id}/messages`

Envia uma mensagem adicional para uma task em estado `input-required`.

**Request:**
```http
POST /a2a/tasks/a2a-task-99x1/messages HTTP/1.1
Host: localhost:7373
Content-Type: application/json

{
  "message": {
    "role": "user",
    "parts": [
      {
        "type": "text",
        "text": "Use a chave pública em /configs/jwt.pub.pem e o algoritmo RS256. O tempo de expiração deve ser 15 minutos."
      }
    ]
  }
}
```

**Response:**
```http
HTTP/1.1 200 OK
Content-Type: application/json

{
  "id": "a2a-task-99x1",
  "status": {
    "state": "working",
    "message": "Informação recebida. Desenvolvedor retomando execução."
  },
  "updatedAt": "2026-05-16T10:08:00Z"
}
```

---

### 5.5 `GET /a2a/tasks/{id}/stream` (SSE)

Stream de eventos em tempo real durante a execução da task.

**Request:**
```http
GET /a2a/tasks/a2a-task-99x1/stream HTTP/1.1
Host: localhost:7373
Accept: text/event-stream
```

**Response:**
```http
HTTP/1.1 200 OK
Content-Type: text/event-stream
Cache-Control: no-cache
Connection: keep-alive

event: state
data: {"state":"working","message":"Arquiteto decompondo tarefa..."}

event: artifact
data: {"name":"dag_plan.json","type":"file","mimeType":"application/json","content":"{...}"}

event: state
data: {"state":"working","message":"Desenvolvedor gerando código..."}

event: state
data: {"state":"completed","message":"Task concluída com sucesso."}

event: done
data: {}
```

**Tipos de eventos SSE:**

| Evento | Descrição |
|--------|-----------|
| `state` | Transição de estado da task |
| `artifact` | Novo artefato gerado (código, log, métrica) |
| `message` | Mensagem do ator (Arquiteto, Desenvolvedor, Inspetor) |
| `error` | Erro não fatal (warning do Hypervisor, rejeição do Inspetor) |
| `done` | Stream finalizado (task em estado terminal) |

---

## 6. Mapeamento de Skills para Atores Internos

Cada skill do AgentCard é mapeada para um pipeline interno de atores:

| Skill | Ator Inicial | Pipeline Completo |
|-------|--------------|-------------------|
| `code-generation` | Arquiteto | Arquiteto → DAG → Desenvolvedor → Inspetor → Hypervisor/Checkpoint |
| `safe-execution` | Hypervisor | Hypervisor → Blackboard (log) |
| `dag-orchestration` | Arquiteto | Arquiteto → DAG → status report |
| `checkpoint-rollback` | Hypervisor | Checkpoint::save / Checkpoint::rollback → git |

Quando uma task é recebida com `skill: "code-generation"`, o A2A Layer:
1. Converte a mensagem do usuário em uma tupla de tarefa no Blackboard.
2. Invoca o ator Arquiteto via `arreio-actors`.
3. Aguarda o DAG ser construído.
4. Executa o scheduling topológico via `arreio-dag`.
5. Para cada nó, invoca o Desenvolvedor, depois o Inspetor.
6. Coleta os artefatos (arquivos gerados) e os anexa à resposta A2A.

---

## 7. Exemplo de Delegação de Tarefa

### 7.1 Cenário: Agente Externo Delega para O Arreio

Um agente A2A externo (por exemplo, um agente de email) detecta uma solicitação de feature e delega ao Arreio.

**Passo 1 — Descoberta do AgentCard:**
```bash
curl http://localhost:7373/a2a/agent-card | jq '.skills[] | select(.id=="code-generation")'
```

**Passo 2 — Criação da Task:**
```bash
curl -X POST http://localhost:7373/a2a/tasks \
  -H "Content-Type: application/json" \
  -d '{
    "message": {
      "role": "user",
      "parts": [{ "type": "text", "text": "Adicione suporte a métricas Prometheus no arreio-gateway" }]
    },
    "skill": "code-generation"
  }'
```

**Resposta:**
```json
{
  "id": "a2a-task-77d2",
  "status": { "state": "submitted", "message": "..." }
}
```

**Passo 3 — Polling de Status:**
```bash
curl http://localhost:7373/a2a/tasks/a2a-task-77d2 | jq '.status.state'
# "working"
# ... (aguardar) ...
# "completed"
```

**Passo 4 — Recuperação de Artefatos:**
```bash
curl http://localhost:7373/a2a/tasks/a2a-task-77d2 | jq '.artifacts[] | {name, metadata}'
```

**Saída:**
```json
{
  "name": "prometheus_metrics.rs",
  "metadata": {
    "lines": 89,
    "tests_passed": 5,
    "inspector_approved": true
  }
}
```

---

### 7.2 Cenário: O Arreio Delega para Agente Externo

O Arreio também pode atuar como cliente A2A. Quando o Arquiteto identifica que uma subtarefa requer uma capacidade não disponível internamente (ex: design de UI/UX), ele pode delegar a um agente externo.

**Exemplo de delegação interna:**
```rust
// Dentro do ator Arquiteto (arreio-actors)
use arreio_gateway::a2a::A2AClient;

let client = A2AClient::discover("http://design-agent.local:8080/a2a/agent-card");
let subtask = client.create_task(
    "Crie um wireframe HTML para o dashboard do Arreio",
    "ui-design"
).await?; // Nota: await aqui é conceitual; implementação real é síncrona com poll

// Aguarda conclusão e recebe o artefato
let artifact = client.poll_for_artifact(&subtask.id, "wireframe.html", Duration::from_secs(120))?;
blackboard.write_tuple(("artifact", subtask.id, artifact.content))?;
```

---

## 8. Formato de Artefatos

Artefatos gerados pelo Arreio seguem o schema A2A de `Artifact` com extensões de metadados.

### 8.1 Artefato de Código

```json
{
  "name": "arreio_provider_streaming.rs",
  "type": "file",
  "mimeType": "text/rust",
  "content": "use std::net::TcpStream;\n// ...",
  "metadata": {
    "language": "rust",
    "lines_of_code": 156,
    "cyclomatic_complexity": 8,
    "test_coverage_percent": 94.5,
    "inspector_issues": {
      "critical": 0,
      "warnings": 1,
      "info": 2
    },
    "hypervisor_exit_code": 0,
    "generated_by": "developer-actor",
    "reviewed_by": "inspector-actor",
    "commit_hash": "a1b2c3d"
  }
}
```

### 8.2 Artefato de Log

```json
{
  "name": "execution_log.json",
  "type": "file",
  "mimeType": "application/json",
  "content": "{\"steps\":[...]}",
  "metadata": {
    "dag_node_count": 5,
    "total_execution_time_ms": 45230,
    "tokens_consumed": 28491,
    "rollback_count": 0
  }
}
```

---

## 9. Erros e Códigos HTTP

| Status HTTP | Código Interno | Descrição |
|-------------|----------------|-----------|
| `200 OK` | — | Operação bem-sucedida |
| `201 Created` | — | Task criada com sucesso |
| `400 Bad Request` | `A2A_INVALID_REQUEST` | JSON malformado ou campo obrigatório ausente |
| `404 Not Found` | `A2A_TASK_NOT_FOUND` | Task ID não existe no Blackboard |
| `409 Conflict` | `A2A_INVALID_STATE_TRANSITION` | Tentativa de enviar mensagem para task não em `input-required` |
| `422 Unprocessable Entity` | `A2A_SKILL_NOT_FOUND` | Skill solicitada não existe no AgentCard |
| `429 Too Many Requests` | `A2A_RATE_LIMIT` | Excedido limite de `maxTasksPerMinute` |
| `500 Internal Server Error` | `A2A_INTERNAL_ERROR` | Falha inesperada no Arreio |
| `503 Service Unavailable` | `A2A_OVERLOAD` | `maxConcurrentTasks` atingido |

---

## 10. Segurança em A2A

### 10.1 Validação de Origem

O Arreio pode ser configurado com uma allowlist de agentes externos:
```toml
# configs/a2a.toml
[a2a.security]
allowed_origins = ["http://claude-desktop.local", "http://cursor.local"]
require_authentication = false  # true em produção
```

### 10.2 Sanitização de Mensagens

Todas as mensagens recebidas via A2A passam pelo McpSandbox (mesmo módulo usado pelo MCP Server):
- Detecção de prompt injection no campo `message.parts[].text`.
- Blocklist de URLs maliciosas em anexos.
- Limite de tamanho: 1MB por mensagem, 10MB por task acumulado.

### 10.3 Isolamento de Tasks

Cada task A2A recebe um namespace isolado no Blackboard:
```
blackboard://a2a/{task_id}/*
```

Isso impede que uma task acesse ou modifique dados de outra task, garantindo isolamento semântico.

---

## 11. Versionamento

- **Versão do protocolo A2A:** 1.0 (Google A2A specification)
- **Versão da implementação Arreio A2A:** 1.0.0
- **Endpoint de compatibilidade:** `GET /a2a/version` retorna `{ "protocol": "1.0", "implementation": "1.0.0" }`

---

## 12. Glossário

| Termo (EN) | Definição (PT) |
|------------|----------------|
| A2A | Agent-to-Agent — protocolo de comunicação entre agentes autônomos |
| AgentCard | Documento JSON que descreve as capacidades de um agente |
| Task | Unidade de trabalho delegada de um agente para outro |
| Artifact | Produto gerado pela execução de uma task (código, log, arquivo) |
| SSE | Server-Sent Events — transporte para streaming de updates |
| Skill | Capacidade especializada anunciada no AgentCard |
| State Transition | Mudança no ciclo de vida de uma task (ex: working → completed) |

---

> **Nota final:** A camada A2A do Arreio é projetada para ser o principal ponto de integração com agentes externos. Qualquer alteração no ciclo de vida da FSM ou no formato do DAG deve ser refletida no mapeamento de estados A2A. Documente alterações no `CHANGELOG` e atualize este documento.
