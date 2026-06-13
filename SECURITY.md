# SECURITY — Políticas de Segurança do O Arreio

> **Versão:** 1.1.0 · **Data:** 2026-06-12
> **Classificação:** Público — diretrizes de deploy e operação
> **Idioma:** Português (texto) / Inglês (termos técnicos e protocolos)
>
> **Status (honesto):** os mecanismos centrais (DLP, blocklist do Hypervisor, audit ledger hash-chain, RBAC, sandbox MCP) **existem no código** e estão cobertos por testes — os caminhos de arquivo foram corrigidos nesta versão para apontar aos crates reais. As seções de **deploy em produção** (mTLS, JWT, WAF, replicação remota) e alguns **comandos de CLI** (`arreio audit …`, `arreio security …`) descrevem **postura pretendida / 🚧 roadmap**, não recursos já expostos. Marcações ao longo do texto.

---

## 1. Visão Geral

A segurança do **O Arreio** é projetada em camadas, seguindo o princípio da **defesa em profundidade** (defense in depth). Como o sistema opera com LLMs locais (Ollama) e executa código gerado automaticamente, a superfície de ataque é significativa. Este documento define as políticas, mecanismos e recomendações para mitigar riscos em quatro dimensões:

1. **Data Loss Prevention (DLP)** — prevenção de vazamento de dados sensíveis.
2. **Leak Prevention** — interceptação proativa de comandos e payloads suspeitos.
3. **Audit Trail** — rastreabilidade completa com integridade criptográfica.
4. **RBAC** — controle de acesso baseado em papéis.
5. **MCP Sandbox** — mitigação específica de tool poisoning no protocolo MCP.

---

## 2. Data Loss Prevention (DLP)

### 2.1 Padrões Detectados

O módulo DLP do Arreio (implementado em `arreio-security/src/dlp.rs`) utiliza regex compilados para detectar padrões sensíveis em:
- Saídas de comandos (`stdout`, `stderr`).
- Conteúdo escrito no Blackboard.
- Mensagens trocadas via MCP e A2A.
- Código gerado antes de ser persistido em disco.

| Categoria | Padrão Regex | Exemplo Detectado | Severidade |
|-----------|--------------|-------------------|------------|
| CPF (BR) | `\b\d{3}\.?\d{3}\.?\d{3}-?\d{2}\b` | `529.982.247-25` | Alto |
| CNPJ (BR) | `\b\d{2}\.?\d{3}\.?\d{3}/?\d{4}-?\d{2}\b` | `12.345.678/0001-90` | Alto |
| Email | `[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}` | `dev@arreio.local` | Médio |
| API Key (genérica) | `(?i)(api[_-]?key|apikey)\s*[:=]\s*['"]?[a-zA-Z0-9]{32,64}['"]?` | `API_KEY=sk_live_51Hx...` | Crítico |
| AWS Access Key | `AKIA[0-9A-Z]{16}` | `AKIAIOSFODNN7EXAMPLE` | Crítico |
| AWS Secret Key | `(?i)aws_secret_access_key\s*[:=]\s*['"]?[a-zA-Z0-9/+=]{40}['"]?` | `aws_secret_access_key=...` | Crítico |
| GitHub Token | `ghp_[a-zA-Z0-9]{36}` | `ghp_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx` | Crítico |
| OpenAI Key | `sk-[a-zA-Z0-9]{48}` | `sk-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx` | Crítico |
| Private Key (PEM) | `-----BEGIN (RSA |EC |DSA |OPENSSH )?PRIVATE KEY-----` | `-----BEGIN RSA PRIVATE KEY-----` | Crítico |
| Senha em URL | `(?i)(password|passwd|pwd)\s*[:=]\s*[^\s&]+` | `password=123456` | Alto |
| Credit Card | `\b(?:4[0-9]{12}(?:[0-9]{3})?|5[1-5][0-9]{14}|3[47][0-9]{13})\b` | `4532015112830366` | Crítico |
| IPv4 interno | `\b(10\.|172\.(1[6-9]|2[0-9]|3[01])\.|192\.168\.)\d+\.\d+\b` | `192.168.1.10` | Baixo |
| Token JWT | `eyJ[a-zA-Z0-9_-]*\.eyJ[a-zA-Z0-9_-]*\.[a-zA-Z0-9_-]*` | `eyJhbGciOiJSUzI1NiIs...` | Médio |

### 2.2 Ações do DLP

Quando um padrão sensível é detectado, o Arreio executa uma ação configurável:

| Ação | Comportamento | Caso de Uso |
|------|---------------|-------------|
| `block` | Interrompe a operação e retorna erro | API keys, private keys |
| `mask` | Substitui o padrão por `***REDACTED***` | CPF, email, tokens JWT |
| `warn` | Permite a operação mas loga alerta | IPs internos, nomes de hosts |
| `quarantine` | Move o conteúdo para área isolada no Blackboard | Código gerado com credenciais hardcoded |

**Exemplo de configuração:**
```toml
# configs/security.toml
[dlp]
enabled = true

[[dlp.rule]]
pattern = "cpf"
severity = "high"
action = "mask"

[[dlp.rule]]
pattern = "api_key"
severity = "critical"
action = "block"

[[dlp.rule]]
pattern = "private_key"
severity = "critical"
action = "block"
```

### 2.3 Pipeline de Detecção

```
Input (stdout/código/mensagem)
    │
    ▼
┌─────────────────────────────┐
│  Normalizador de Encoding   │  // Remove BOM, normaliza UTF-8, decoda base64 superficial
└─────────────┬───────────────┘
              │
              ▼
┌─────────────────────────────┐
│  Scanner Regex Paralelo     │  // Cada padrão em um thread do pool Rayon-like (síncrono com chunks)
│  (crossbeam::scope)         │
└─────────────┬───────────────┘
              │
              ▼
┌─────────────────────────────┐
│  Classificador de Severidade│  // Prioriza critical > high > medium > low
└─────────────┬───────────────┘
              │
    ┌─────────┴─────────┐
    ▼                   ▼
 Ação: block        Ação: mask
    │                   │
    ▼                   ▼
 Erro retornado    Conteúdo sanitizado
 ao cliente        prossegue
```

---

## 3. Leak Prevention (Interceptação Proativa)

O **Leak Prevention** atua antes mesmo do DLP — ele intercepta comandos e operações que poderiam causar vazamento de dados ou modificação não autorizada.

### 3.1 Interceptação de Comandos (Hypervisor)

O Hypervisor (`arreio-hypervisor/src/sandbox.rs`, com regras de shell em `bash_security.rs`) mantém uma **blocklist regex** de comandos que nunca devem ser executados, independentemente do contexto:

```rust
pub const COMMAND_BLOCKLIST: &[&str] = &[
    // Destruição de dados
    r"(?i)rm\s+-rf\s+/",
    r"(?i)rm\s+-rf\s+~",
    r"(?i)rm\s+-rf\s+\\",
    r"(?i)format\s+",
    r"(?i)mkfs\.",
    r"(?i)dd\s+if=.+of=/dev/",

    // Privilege escalation
    r"(?i)chmod\s+777\s+",
    r"(?i)chmod\s+-R\s+777",
    r"(?i)chown\s+-R\s+root",
    r"(?i)sudo\s+.*rm",

    // Execução remota não auditada
    r"(?i)curl\s+.*\|\s*sh",
    r"(?i)curl\s+.*\|\s*bash",
    r"(?i)wget\s+.*\|\s*sh",
    r"(?i)fetch\s+.*\|\s*sh",

    // Banco de dados destrutivo
    r"(?i)DROP\s+DATABASE",
    r"(?i)DROP\s+TABLE",
    r"(?i)DELETE\s+FROM\s+.*WHERE\s+1\s*=\s*1",

    // Exfiltração de rede
    r"(?i)nc\s+-.*\d+\.\d+\.\d+\.\d+",
    r"(?i)netcat\s+.*\d+\.\d+\.\d+\.\d+",
    r"(?i)scp\s+.*@",
    r"(?i)rsync\s+.*@",

    // Modificação de configuração de segurança
    r"(?i)ufw\s+disable",
    r"(?i)iptables\s+-F",
    r"(?i)systemctl\s+stop\s+firewalld",
];
```

### 3.2 Interceptação de Escrita no Blackboard

Antes de qualquer `blackboard_write`, o Leak Prevention verifica:
1. Se a tupla contém campos que parecem credenciais (via DLP regex).
2. Se a tupla sobrescreve dados críticos do sistema (ex: estado da FSM, configurações de segurança).
3. Se o tamanho da tupla excede o limite (1MB por tupla, 10MB por namespace).

### 3.3 Interceptação de Leitura

O Leak Prevention também monitora leituras proibidas:
- Arquivos fora do working directory (`..`, `/etc`, `C:\Windows`).
- Arquivos de configuração sensíveis (`.env`, `id_rsa`, `*.pem`, `shadow`, `SAM`).
- Chaves de registro do Windows relacionadas a credenciais.

**Exemplo de bloqueio de leitura:**
```
[LEAK-PREVENTION] Leitura bloqueada: path "../.env" viola a política de sandbox.
Agente: developer-actor
Task: task-02a4
Ação: erro retornado ao ator, task movida para Correction
```

---

## 4. Audit Trail (Trilha de Auditoria)

O Audit Trail do Arreio garante **integridade criptográfica encadeada** — cada evento de auditoria contém o hash do evento anterior, formando uma cadeia imutável semelhante a um blockchain simplificado.

### 4.1 Estrutura do Evento de Auditoria

```json
{
  "seq": 1847,
  "timestamp": "2026-05-16T10:08:51.364Z",
  "level": "INFO",
  "category": "TASK_EXECUTION",
  "actor": "developer-actor",
  "task_id": "task-02a4",
  "action": "safe_execute",
  "details": {
    "command": "cargo test -p arreio-kernel",
    "exit_code": 0,
    "duration_ms": 8432
  },
  "prev_hash": "a3f7b2c...e9d1",
  "this_hash": "8e4c2a1...f5b3"
}
```

### 4.2 Cálculo do Hash Encadeado

```rust
use sha2::{Sha256, Digest};

fn compute_hash(seq: u64, timestamp: &str, prev_hash: &str, details: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(seq.to_le_bytes());
    hasher.update(timestamp.as_bytes());
    hasher.update(prev_hash.as_bytes());
    hasher.update(details.as_bytes());
    format!("{:x}", hasher.finalize())
}
```

### 4.3 Persistência

O Audit Trail é persistido em:
1. **Arquivo local append-only:** `logs/audit/audit.chain` (modo append-only via permissões de arquivo).
2. **Blackboard:** tupla `("audit", seq, json_event)` para consulta rápida.
3. **Snapshot periódico:** a cada 1000 eventos, um snapshot com hash merkle é gerado.

### 4.4 Verificação de Integridade

> 🚧 **Roadmap:** um subcomando `arreio audit verify` para verificar a cadeia ainda **não existe** no CLI. A integridade hoje é validada pelos testes do crate (`cargo test -p arreio-security`) e pela verificação do hash encadeado em código. A saída abaixo é o **formato pretendido** desse comando futuro:

```text
# (formato pretendido — comando ainda não implementado)
Verificando cadeia de auditoria...
Eventos: 1847
Hash inicial: 0000...0000 (genesis)
Hash final: 8e4c2a1...f5b3
Integridade: OK (nenhuma quebra detectada)
```

### 4.5 Eventos Auditados

| Categoria | Eventos Incluídos |
|-----------|-------------------|
| `AUTH` | Login, troca de role, falha de autenticação |
| `TASK_LIFECYCLE` | Criação, transição de estado, conclusão, falha |
| `BLACKBOARD_ACCESS` | Leitura, escrita, deleção de tupla |
| `HYPERVISOR` | Execução de comando, bloqueio, timeout, rollback |
| `MCP` | Chamada de tool, subscrição de resource, get prompt |
| `A2A` | Criação de task, delegação, recebimento de artefato |
| `DLP` | Detecção de padrão sensível, ação de mask/block |
| `CHECKPOINT` | Git commit, git reset, snapshot do Blackboard |

---

## 5. RBAC (Role-Based Access Control)

### 5.1 Papéis Definidos

| Papel | Permissões | Escopo |
|-------|------------|--------|
| `admin` | Todas as operações | Global |
| `operator` | create_task, safe_execute, blackboard_read, dag_status | Global |
| `developer` | create_task, safe_execute (readonly em blackboard_write), dag_status | Workspace |
| `auditor` | blackboard_read, audit_read, dag_status | Global (readonly) |
| `guest` | blackboard_read (namespace público apenas), dag_status | Público |

### 5.2 Matriz de Permissões

| Operação | admin | operator | developer | auditor | guest |
|----------|-------|----------|-----------|---------|-------|
| `create_task` | ✓ | ✓ | ✓ | ✗ | ✗ |
| `checkpoint_rollback` | ✓ | ✓ | ✗ | ✗ | ✗ |
| `safe_execute` | ✓ | ✓ | ✓ (limitado) | ✗ | ✗ |
| `blackboard_read` | ✓ | ✓ | ✓ | ✓ | ✓ (parcial) |
| `blackboard_write` | ✓ | ✓ | ✓ (namespace próprio) | ✗ | ✗ |
| `dag_status` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `audit_read` | ✓ | ✗ | ✗ | ✓ | ✗ |
| `prompt_get` | ✓ | ✓ | ✓ | ✓ | ✗ |

### 5.3 Controle de Acesso no Código

```rust
// arreio-security/src/rbac.rs  (papéis reais: Admin, Developer, Auditor, Guest)
pub fn check_permission(role: Role, operation: Operation) -> Result<(), RbacError> {
    let matrix = match role {
        Role::Admin => return Ok(()),
        Role::Operator => OPERATOR_MATRIX,
        Role::Developer => DEVELOPER_MATRIX,
        Role::Auditor => AUDITOR_MATRIX,
        Role::Guest => GUEST_MATRIX,
    };
    if matrix.contains(&operation) {
        Ok(())
    } else {
        Err(RbacError::InsufficientPermissions { role, operation })
    }
}
```

### 5.4 Autenticação

Em ambientes de produção, o Arreio suporta:
- **API Key:** header `X-Arreio-Key` com HMAC-SHA256 do timestamp.
- **mTLS:** certificados de cliente para conexões MCP/A2A via TLS (quando implementado no gateway).
- **Token JWT:** emitido por um IdP externo, validado via chave pública RS256.

---

## 6. MCP Sandbox

O **McpSandbox** é o módulo de segurança específico para mitigar ataques de **tool poisoning** no protocolo MCP.

### 6.1 Vetores de Ataque Mitigados

| Ataque | Descrição | Mitigação |
|--------|-----------|-----------|
| **Tool Poisoning** | A description da tool contém instruções maliciosas | Validação regex de descriptions antes do handshake |
| **Prompt Injection via Args** | Argumentos da tool contêm instruções para o LLM | Sanitização de Unicode homoglyphs e escape sequences |
| **Argument Smuggling** | Payload codificado (base64, URL encoding) em argumentos | Decodificação superficial e re-análise |
| **Description Overflow** | Description extremamente longa para confundir o LLM | Limite de 500 caracteres por description |
| **Nested JSON Injection** | Argumento JSON contém campos não esperados pelo schema | Validação estrita via `serde_json` com `deny_unknown_fields` |

### 6.2 Validação de Descriptions

```rust
// arreio-mcp-server/src/sandbox.rs
const MAX_DESCRIPTION_LEN: usize = 500;
const FORBIDDEN_DESCRIPTION_PATTERNS: &[&str] = &[
    r"(?i)ignore\s+(all\s+)?previous\s+instructions",
    r"(?i)disregard\s+(the\s+)?system\s+prompt",
    r"(?i)you\s+are\s+now\s+.*(?:admin|root|superuser)",
    r"(?i)send\s+(the\s+)?(data|file|content)\s+to\s+http",
    r"(?i)execute\s+this\s+command\s+instead",
    r"(?i)overwrite\s+(the\s+)?(?:security|safety|policy)",
];

pub fn validate_tool(tool: &Tool) -> Result<(), SandboxError> {
    if tool.description.len() > MAX_DESCRIPTION_LEN {
        return Err(SandboxError::DescriptionTooLong);
    }
    for pat in FORBIDDEN_DESCRIPTION_PATTERNS {
        if Regex::new(pat)?.is_match(&tool.description) {
            return Err(SandboxError::PoisoningDetected(pat.to_string()));
        }
    }
    Ok(())
}
```

### 6.3 Rate Limiting por Sessão

```rust
pub struct SessionRateLimiter {
    max_calls_per_minute: u32,
    max_bytes_per_minute: u64,
    calls: Vec<Instant>,
    bytes: Vec<(Instant, u64)>,
}

impl SessionRateLimiter {
    pub fn check(&mut self, call_bytes: u64) -> Result<(), RateLimitError> {
        let now = Instant::now();
        let window = now - Duration::from_secs(60);

        self.calls.retain(|&t| t > window);
        self.bytes.retain(|(t, _)| *t > window);

        if self.calls.len() >= self.max_calls_per_minute as usize {
            return Err(RateLimitError::TooManyCalls);
        }

        let total_bytes: u64 = self.bytes.iter().map(|(_, b)| b).sum();
        if total_bytes + call_bytes > self.max_bytes_per_minute {
            return Err(RateLimitError::BandwidthExceeded);
        }

        self.calls.push(now);
        self.bytes.push((now, call_bytes));
        Ok(())
    }
}
```

---

## 7. Recomendações de Deploy Seguro

### 7.1 Ambiente de Desenvolvimento

```
┌─────────────────────────────────────────┐
│  Estação de Desenvolvimento             │
│  • Ollama em 127.0.0.1:11434            │
│  • O Arreio em modo single-user          │
│  • Sem autenticação (RBAC: developer)   │
│  • Blackboard em JSON local             │
│  • Hypervisor com blocklist padrão      │
│  • DLP em modo warn                     │
└─────────────────────────────────────────┘
```

### 7.2 Ambiente de Staging

```
┌─────────────────────────────────────────┐
│  Servidor de Staging                    │
│  • Ollama em rede interna (não exposto) │
│  • O Arreio com RBAC ativo               │
│  • API Key ou mTLS para MCP/A2A         │
│  • Blackboard em volume persistente     │
│  • DLP em modo mask                     │
│  • Audit Trail ativo e verificado       │
│  • Checkpoint git em branch staging     │
└─────────────────────────────────────────┘
```

### 7.3 Ambiente de Produção (Recomendado)

```
┌─────────────────────────────────────────┐
│  Servidor de Produção                   │
│  • Ollama isolado em container/VLAN     │
│  • O Arreio com RBAC full + JWT          │
│  • mTLS obrigatório para todo tráfego   │
│  • DLP em modo block para critical      │
│  • Leak Prevention com blocklist custom │
│  • Audit Trail com replicação remota    │
│  • Backup do Blackboard a cada 5 min    │
│  • WAF/reverse proxy na frente do gw    │
└─────────────────────────────────────────┘
```

### 7.4 Checklist de Hardening

- [ ] Alterar a porta padrão do gateway (7373) para uma porta não padrão.
- [ ] Desabilitar o transporte stdio em produção (usar apenas HTTP + mTLS).
- [ ] Configurar `ulimit` adequado para prevenir fork bombs no Hypervisor.
- [ ] Habilitar ASLR e DEP no sistema operacional host.
- [ ] Isolar o diretório de build (`C:\dev\arreio-target`) fora de paths sincronizados (OneDrive, Dropbox).
- [ ] Revisar manualmente a blocklist do Hypervisor a cada release.
- [ ] Executar `cargo audit` para detectar vulnerabilidades em dependências.
- [ ] Manter o `vendor/zmij` atualizado e auditado.
- [ ] Configurar log rotation para `logs/audit/audit.chain`.
- [ ] Testar o procedimento de rollback antes de colocar em produção.

### 7.5 Verificação de Segurança

Hoje a verificação de segurança é feita pelos testes dos crates (caminho real e verificável):

```bash
# Testa o Hypervisor (blocklist/sandbox) contra comandos maliciosos
cargo test -p arreio-hypervisor

# Testa DLP, RBAC e audit ledger
cargo test -p arreio-security

# Testa o sandbox MCP (tool poisoning / rate limit)
cargo test -p arreio-mcp-server
```

> 🚧 **Roadmap:** subcomandos agregadores `arreio security audit` e `arreio security dlp-status` ainda **não existem** no CLI. Quando existirem, encapsularão as verificações acima num só comando.

---

## 8. Incident Response

### 8.1 Classificação de Incidentes

| Severidade | Critérios | Tempo de Resposta |
|------------|-----------|-------------------|
| P1 — Crítico | Exfiltração confirmada, execução de código malicioso, private key exposta | Imediato |
| P2 — Alto | DLP detectou API key em stdout, tentativa de privilege escalation | 1 hora |
| P3 — Médio | Rate limit excedido repetidamente, pattern suspeito em argumento | 4 horas |
| P4 — Baixo | Alerta de leitura fora do escopo, tentativa de acesso negado por RBAC | 24 horas |

### 8.2 Playbook de Resposta a Exfiltração

1. **Detecção:** DLP detecta API key em stdout de `safe_execute`.
2. **Contenção:** Hypervisor mata o processo imediatamente (exit code -9).
3. **Evidência:** Audit Trail captura o evento com hash encadeado.
4. **Isolamento:** Task é movida para `StrategicRetreat`, FSM bloqueia novas execuções.
5. **Rotação:** Operador humano deve rotacionar a credencial exposta.
6. **Análise:** Revisar o código que gerou o stdout (provavelmente do ator Desenvolvedor).
7. **Correção:** Ajustar prompt do Desenvolvedor para nunca logar credenciais.
8. **Retomada:** Após aprovação manual, `arreio resume` continua o pipeline.

---

## 9. Glossário

| Termo (EN) | Definição (PT) |
|------------|----------------|
| DLP | Data Loss Prevention — prevenção de vazamento de dados sensíveis |
| Leak Prevention | Interceptação proativa de operações que poderiam vazar dados |
| Audit Trail | Registro imutável e encadeado de todos os eventos do sistema |
| RBAC | Role-Based Access Control — controle de acesso por papéis |
| MCP Sandbox | Módulo de mitigação de tool poisoning no protocolo MCP |
| Tool Poisoning | Ataque onde a descrição de uma tool MCP é manipulada maliciosamente |
| Blocklist | Lista de padrões proibidos (regex) para comandos ou conteúdos |
| Rate Limiting | Limitação de chamadas por unidade de tempo |
| mTLS | Mutual TLS — autenticação via certificados de cliente e servidor |
| HMAC | Hash-based Message Authentication Code — assinatura de requisições |

---

> **Nota final:** A segurança do Arreio é um processo contínuo. Cada novo ator, tool ou endpoint exposto deve passar por uma revisão de segurança antes do merge. Atualize este documento e o `AGENTS.md` sempre que novos vetores de ataque forem identificados ou novas mitigações forem implementadas.
