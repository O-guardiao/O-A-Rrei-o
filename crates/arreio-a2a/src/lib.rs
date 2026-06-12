//! Arreio-A2A — Compatibilidade com Agent-to-Agent Protocol (Google/LF).
//!
//! Permite que O Arreio receba delegações de outros agentes via HTTP.
//! Servidor síncrono embutido (TcpListener + threads), sem frameworks externos.

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

// ── AgentCard ─────────────────────────────────────────────────────────────────

/// Esquema de autenticação suportado pelo agente.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AuthScheme {
    None,
    Bearer,
    ApiKey,
}

/// Capacidade individual exposta no AgentCard.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Capability {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Value,
}

/// Cartão de agente A2A: descreve capacidades e endpoints do Arreio.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AgentCard {
    pub name: String,
    pub version: String,
    pub capabilities: Vec<Capability>,
    pub endpoints: Vec<String>,
    pub authentication: Option<AuthScheme>,
}

impl AgentCard {
    pub fn new() -> Self {
        Self {
            name: "O Arreio".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: vec![
                Capability {
                    name: "blackboard".to_string(),
                    description: "Acesso ao estado central compartilhado (Tuple Space + Pub/Sub)"
                        .to_string(),
                    input_schema: serde_json::json!({"type": "object"}),
                    output_schema: serde_json::json!({"type": "object"}),
                },
                Capability {
                    name: "dag".to_string(),
                    description: "Orquestração de tarefas via grafo acíclico dirigido".to_string(),
                    input_schema: serde_json::json!({"type": "array", "items": {"type": "string"}}),
                    output_schema: serde_json::json!({"type": "object", "properties": {"status": {"type": "string"}}}),
                },
                Capability {
                    name: "fsm".to_string(),
                    description: "Máquina de estado finito com transições validadas".to_string(),
                    input_schema: serde_json::json!({"type": "string", "enum": ["Idle", "Exploration", "Planning", "Execution", "Evaluation"]}),
                    output_schema: serde_json::json!({"type": "string"}),
                },
                Capability {
                    name: "hypervisor".to_string(),
                    description: "Sandbox de processos com interceptor e watchdog".to_string(),
                    input_schema: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
                    output_schema: serde_json::json!({"type": "object", "properties": {"exit_code": {"type": "integer"}}}),
                },
                Capability {
                    name: "checkpoint".to_string(),
                    description: "Snapshots e rollback automático via git".to_string(),
                    input_schema: serde_json::json!({"type": "null"}),
                    output_schema: serde_json::json!({"type": "string"}),
                },
            ],
            endpoints: vec!["http://127.0.0.1:8080/a2a".to_string()],
            authentication: Some(AuthScheme::Bearer),
        }
    }
}

impl Default for AgentCard {
    fn default() -> Self {
        Self::new()
    }
}

// ── Task Lifecycle ────────────────────────────────────────────────────────────

/// Estados possíveis de uma tarefa A2A.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TaskState {
    Submitted,
    Working,
    InputRequired,
    Completed,
    Failed,
    Cancelled,
}

/// Metadados de uma tarefa A2A.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TaskMetadata {
    pub requested_by: String,
    pub priority: u8,
    pub checkpoint_id: Option<String>,
}

impl Default for TaskMetadata {
    fn default() -> Self {
        Self {
            requested_by: "anonymous".to_string(),
            priority: 5,
            checkpoint_id: None,
        }
    }
}

/// Artefato produzido pela execução de uma tarefa.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Artifact {
    pub content: String,
    pub mime_type: String,
    pub checkpoint_id: Option<String>,
}

/// Tarefa delegada por outro agente.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Task {
    pub id: String,
    pub state: TaskState,
    pub spec: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub artifacts: Vec<Artifact>,
    pub metadata: TaskMetadata,
}

impl Task {
    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    /// Cria uma nova tarefa no estado Submitted.
    pub fn new(
        id: impl Into<String>,
        spec: impl Into<String>,
        requested_by: impl Into<String>,
    ) -> Self {
        let now = Self::now();
        Self {
            id: id.into(),
            state: TaskState::Submitted,
            spec: spec.into(),
            created_at: now,
            updated_at: now,
            artifacts: Vec::new(),
            metadata: TaskMetadata {
                requested_by: requested_by.into(),
                ..Default::default()
            },
        }
    }
}

// ── TaskManager ───────────────────────────────────────────────────────────────

/// Gerenciador em-memória de tarefas A2A.
pub struct TaskManager {
    tasks: HashMap<String, Task>,
    on_submit: Option<Box<dyn FnMut(&Task) -> Result<()> + Send>>,
}

impl TaskManager {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            on_submit: None,
        }
    }

    /// Registra um callback a ser executado quando uma tarefa for submetida.
    /// Se o callback retornar Ok, o estado da tarefa é promovido de Submitted para Working.
    pub fn attach_dag_callback<F>(&mut self, callback: F)
    where
        F: FnMut(&Task) -> Result<()> + Send + 'static,
    {
        self.on_submit = Some(Box::new(callback));
    }

    /// Submete uma nova tarefa e retorna a referência criada.
    /// Caso um callback DAG esteja registrado, invoca-o e promove o estado para Working em caso de sucesso.
    pub fn submit(&mut self, spec: &str, requested_by: &str) -> Task {
        let id = format!("task-{}", uuid::Uuid::new_v4());
        let mut task = Task::new(&id, spec, requested_by);
        self.tasks.insert(id.clone(), task.clone());

        if let Some(ref mut cb) = self.on_submit {
            match cb(&task) {
                Ok(()) => {
                    task.state = TaskState::Working;
                    task.updated_at = Task::now();
                    if let Some(t) = self.tasks.get_mut(&id) {
                        t.state = TaskState::Working;
                        t.updated_at = task.updated_at;
                    }
                }
                Err(e) => {
                    eprintln!("[a2a] callback de DAG falhou para {}: {}", id, e);
                }
            }
        }

        task
    }

    /// Atualiza o estado de uma tarefa existente.
    pub fn update_state(&mut self, task_id: &str, state: TaskState) -> Result<()> {
        let task = self
            .tasks
            .get_mut(task_id)
            .with_context(|| format!("tarefa não encontrada: {}", task_id))?;
        task.state = state;
        task.updated_at = Task::now();
        Ok(())
    }

    /// Adiciona um artefato a uma tarefa existente.
    pub fn add_artifact(&mut self, task_id: &str, artifact: Artifact) -> Result<()> {
        let task = self
            .tasks
            .get_mut(task_id)
            .with_context(|| format!("tarefa não encontrada: {}", task_id))?;
        task.artifacts.push(artifact);
        task.updated_at = Task::now();
        Ok(())
    }

    /// Retorna uma referência para a tarefa, se existir.
    pub fn get_task(&self, task_id: &str) -> Option<&Task> {
        self.tasks.get(task_id)
    }

    /// Lista todas as tarefas em um determinado estado.
    pub fn list_by_state(&self, state: TaskState) -> Vec<&Task> {
        self.tasks.values().filter(|t| t.state == state).collect()
    }

    /// Retorna todas as tarefas ordenadas por updated_at decrescente.
    pub fn list_all(&self) -> Vec<&Task> {
        let mut v: Vec<&Task> = self.tasks.values().collect();
        v.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        v
    }
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

// ── Servidor HTTP A2A ─────────────────────────────────────────────────────────

/// Inicializa o servidor A2A (bloqueante).
pub fn serve_a2a(addr: &str, manager: Arc<Mutex<TaskManager>>) -> Result<()> {
    let listener = TcpListener::bind(addr).with_context(|| format!("falha ao bind em {}", addr))?;
    println!("[a2a] ouvindo em http://{}", addr);

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let mgr = Arc::clone(&manager);
                std::thread::spawn(move || {
                    let _ = handle_a2a_connection(stream, mgr);
                });
            }
            Err(e) => eprintln!("[a2a] erro de conexão: {}", e),
        }
    }
    Ok(())
}

fn handle_a2a_connection(mut stream: TcpStream, manager: Arc<Mutex<TaskManager>>) -> Result<()> {
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    let mut reader = BufReader::new(&stream);

    // Lê a primeira linha: METHOD PATH HTTP/1.1
    let mut first_line = String::new();
    reader.read_line(&mut first_line)?;
    let parts: Vec<&str> = first_line.trim().split_whitespace().collect();
    if parts.len() < 2 {
        return send_json_response(&mut stream, 400, r#"{"error":"bad request"}"#);
    }
    let method = parts[0];
    let path = parts[1];

    // Lê headers até linha em branco
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line.trim().is_empty() {
            break;
        }
        headers.push(line);
    }

    // Lê body se Content-Length presente
    let mut body = String::new();
    for h in &headers {
        if h.to_lowercase().starts_with("content-length:") {
            if let Some(len_str) = h.split(':').nth(1) {
                if let Ok(len) = len_str.trim().parse::<usize>() {
                    let mut buf = vec![0u8; len];
                    reader.read_exact(&mut buf)?;
                    body = String::from_utf8_lossy(&buf).into_owned();
                }
            }
        }
    }

    match (method, path) {
        ("GET", "/a2a/agent-card") => route_agent_card(&mut stream),
        ("POST", "/a2a/tasks") => route_create_task(&mut stream, &body, manager),
        ("GET", path) if path.starts_with("/a2a/tasks/") && !path.contains("/artifacts") => {
            let id = path.strip_prefix("/a2a/tasks/").unwrap_or("");
            route_get_task(&mut stream, id, manager)
        }
        ("POST", path) if path.ends_with("/artifacts") => {
            let prefix = "/a2a/tasks/";
            let suffix = "/artifacts";
            if let Some(mid) = path
                .strip_prefix(prefix)
                .and_then(|s| s.strip_suffix(suffix))
            {
                route_add_artifact(&mut stream, mid, &body, manager)
            } else {
                send_json_response(&mut stream, 404, r#"{"error":"not found"}"#)
            }
        }
        ("GET", "/a2a/tasks") => route_list_tasks(&mut stream, manager),
        _ => send_json_response(&mut stream, 404, r#"{"error":"not found"}"#),
    }
}

// ── Rotas A2A ─────────────────────────────────────────────────────────────────

fn route_agent_card(stream: &mut TcpStream) -> Result<()> {
    let card = AgentCard::new();
    let json = serde_json::to_string(&card)?;
    send_json_response(stream, 200, &json)
}

fn route_create_task(
    stream: &mut TcpStream,
    body: &str,
    manager: Arc<Mutex<TaskManager>>,
) -> Result<()> {
    let payload: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return send_json_response(stream, 400, r#"{"error":"invalid json"}"#),
    };

    let spec = payload.get("spec").and_then(|v| v.as_str()).unwrap_or("");
    let requested_by = payload
        .get("requested_by")
        .and_then(|v| v.as_str())
        .unwrap_or("anonymous");

    let mut mgr = manager.lock().unwrap();
    let task = mgr.submit(spec, requested_by);
    let json = serde_json::to_string(&task)?;
    send_json_response(stream, 201, &json)
}

fn route_get_task(
    stream: &mut TcpStream,
    task_id: &str,
    manager: Arc<Mutex<TaskManager>>,
) -> Result<()> {
    let mgr = manager.lock().unwrap();
    match mgr.get_task(task_id) {
        Some(task) => {
            let json = serde_json::to_string(task)?;
            send_json_response(stream, 200, &json)
        }
        None => send_json_response(stream, 404, r#"{"error":"task not found"}"#),
    }
}

fn route_add_artifact(
    stream: &mut TcpStream,
    task_id: &str,
    body: &str,
    manager: Arc<Mutex<TaskManager>>,
) -> Result<()> {
    let payload: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return send_json_response(stream, 400, r#"{"error":"invalid json"}"#),
    };

    let content = payload
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mime_type = payload
        .get("mime_type")
        .and_then(|v| v.as_str())
        .unwrap_or("text/plain")
        .to_string();
    let checkpoint_id = payload
        .get("checkpoint_id")
        .and_then(|v| v.as_str())
        .map(String::from);

    let artifact = Artifact {
        content,
        mime_type,
        checkpoint_id,
    };

    let mut mgr = manager.lock().unwrap();
    match mgr.add_artifact(task_id, artifact) {
        Ok(_) => send_json_response(stream, 200, r#"{"status":"artifact added"}"#),
        Err(e) => send_json_response(stream, 404, &format!(r#"{{"error":"{}"}}"#, e)),
    }
}

fn route_list_tasks(stream: &mut TcpStream, manager: Arc<Mutex<TaskManager>>) -> Result<()> {
    let mgr = manager.lock().unwrap();
    let tasks = mgr.list_all();
    let json = serde_json::to_string(&tasks)?;
    send_json_response(stream, 200, &json)
}

// ── Utilitários HTTP ──────────────────────────────────────────────────────────

fn send_json_response(stream: &mut TcpStream, status: u16, body: &str) -> Result<()> {
    let status_text = match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Unknown",
    };
    let response = format!(
        "HTTP/1.1 {} {}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         \r\n\
         {}",
        status,
        status_text,
        body.len(),
        body
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()?;
    let _ = stream.shutdown(Shutdown::Both);
    Ok(())
}

// ── API de conveniência (wrapper para integração com arreio-cli) ────────────────

/// Inicializa o servidor A2A com um TaskManager próprio (bloqueante).
pub fn init_a2a_server() -> Result<()> {
    let manager = Arc::new(Mutex::new(TaskManager::new()));
    serve_a2a("127.0.0.1:8080", manager)
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn agent_card_possui_capacidades_com_schema() {
        let card = AgentCard::new();
        assert_eq!(card.name, "O Arreio");
        assert!(!card.capabilities.is_empty());
        let cap = &card.capabilities[0];
        assert!(!cap.name.is_empty());
        assert!(cap.input_schema.is_object());
        assert!(cap.output_schema.is_object());
        assert_eq!(card.authentication, Some(AuthScheme::Bearer));
    }

    #[test]
    fn task_state_ciclo_completo() {
        let t = Task::new("t1", "teste", "agent-x");
        assert_eq!(t.state, TaskState::Submitted);
        assert_eq!(t.metadata.requested_by, "agent-x");
        assert_eq!(t.metadata.priority, 5);
        assert!(t.artifacts.is_empty());
        assert!(t.created_at > 0);
    }

    #[test]
    fn artifact_com_mime_type() {
        let a = Artifact {
            content: "resultado".to_string(),
            mime_type: "application/json".to_string(),
            checkpoint_id: Some("cp-1".to_string()),
        };
        assert_eq!(a.mime_type, "application/json");
        assert_eq!(a.checkpoint_id, Some("cp-1".to_string()));
    }

    #[test]
    fn task_manager_submit_e_recupera() {
        let mut mgr = TaskManager::new();
        let task = mgr.submit("especificaçao", "agent-y");
        let recuperado = mgr.get_task(&task.id).unwrap();
        assert_eq!(recuperado.spec, "especificaçao");
        assert_eq!(recuperado.metadata.requested_by, "agent-y");
    }

    #[test]
    fn task_manager_callback_promove_para_working() {
        use std::sync::{Arc, Mutex};
        let flag = Arc::new(Mutex::new(false));
        let flag2 = Arc::clone(&flag);
        let mut mgr = TaskManager::new();
        mgr.attach_dag_callback(move |task| {
            *flag2.lock().unwrap() = true;
            assert_eq!(task.spec, "spec-com-callback");
            Ok(())
        });
        let task = mgr.submit("spec-com-callback", "agent-callback");
        assert!(*flag.lock().unwrap());
        assert_eq!(task.state, TaskState::Working);
        let t = mgr.get_task(&task.id).unwrap();
        assert_eq!(t.state, TaskState::Working);
    }

    #[test]
    fn task_manager_callback_falha_mantem_submitted() {
        let mut mgr = TaskManager::new();
        mgr.attach_dag_callback(move |_task| Err(anyhow::anyhow!("erro simulado")));
        let task = mgr.submit("spec-falha", "agent-falha");
        assert_eq!(task.state, TaskState::Submitted);
        let t = mgr.get_task(&task.id).unwrap();
        assert_eq!(t.state, TaskState::Submitted);
    }

    #[test]
    fn task_manager_atualiza_estado() {
        let mut mgr = TaskManager::new();
        let task = mgr.submit("spec", "agent-z");
        mgr.update_state(&task.id, TaskState::Working).unwrap();
        let t = mgr.get_task(&task.id).unwrap();
        assert_eq!(t.state, TaskState::Working);
    }

    #[test]
    fn task_manager_estado_inexistente_retorna_erro() {
        let mut mgr = TaskManager::new();
        let err = mgr.update_state("inexistente", TaskState::Completed);
        assert!(err.is_err());
    }

    #[test]
    fn task_manager_adiciona_artifact() {
        let mut mgr = TaskManager::new();
        let task = mgr.submit("spec", "agent-w");
        let art = Artifact {
            content: "código".to_string(),
            mime_type: "text/plain".to_string(),
            checkpoint_id: None,
        };
        mgr.add_artifact(&task.id, art).unwrap();
        let t = mgr.get_task(&task.id).unwrap();
        assert_eq!(t.artifacts.len(), 1);
        assert_eq!(t.artifacts[0].content, "código");
    }

    #[test]
    fn task_manager_lista_por_estado() {
        let mut mgr = TaskManager::new();
        let t1 = mgr.submit("s1", "a1");
        let _t2 = mgr.submit("s2", "a2");
        mgr.update_state(&t1.id, TaskState::Working).unwrap();
        let working = mgr.list_by_state(TaskState::Working);
        let submitted = mgr.list_by_state(TaskState::Submitted);
        assert_eq!(working.len(), 1);
        assert_eq!(submitted.len(), 1);
    }

    #[test]
    fn task_manager_list_all_ordenado() {
        let mut mgr = TaskManager::new();
        let t1 = mgr.submit("s1", "a1");
        thread::sleep(Duration::from_millis(10));
        let t2 = mgr.submit("s2", "a2");
        let all = mgr.list_all();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, t2.id); // mais recente primeiro
        assert_eq!(all[1].id, t1.id);
    }

    #[test]
    fn servidor_responde_agent_card() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let addr = format!("127.0.0.1:{}", port);
        let mgr_clone = Arc::clone(&manager);
        thread::spawn(move || {
            serve_a2a(&addr, mgr_clone).unwrap();
        });
        thread::sleep(Duration::from_millis(100));

        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let req = "GET /a2a/agent-card HTTP/1.1\r\nHost: localhost\r\n\r\n";
        stream.write_all(req.as_bytes()).unwrap();
        stream.flush().unwrap();

        let mut buf = [0u8; 2048];
        let n = stream.read(&mut buf).unwrap();
        let resp = String::from_utf8_lossy(&buf[..n]);
        assert!(resp.contains("200 OK"), "resp: {}", resp);
        assert!(resp.contains("O Arreio"), "resp: {}", resp);
        assert!(resp.contains("blackboard"), "resp: {}", resp);
    }

    #[test]
    fn servidor_cria_e_recupera_tarefa() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let addr = format!("127.0.0.1:{}", port);
        let mgr_clone = Arc::clone(&manager);
        thread::spawn(move || {
            serve_a2a(&addr, mgr_clone).unwrap();
        });
        thread::sleep(Duration::from_millis(100));

        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let body = r#"{"spec":"fazer deploy","requested_by":"agent-bravo"}"#;
        let req = format!(
            "POST /a2a/tasks HTTP/1.1\r\n\
             Host: localhost\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             \r\n\
             {}",
            body.len(),
            body
        );
        stream.write_all(req.as_bytes()).unwrap();
        stream.flush().unwrap();

        let mut buf = [0u8; 2048];
        let n = stream.read(&mut buf).unwrap();
        let resp = String::from_utf8_lossy(&buf[..n]);
        assert!(resp.contains("201 Created"), "resp: {}", resp);

        // Extrai id da tarefa criada
        let body_start = resp.find("\r\n\r\n").unwrap() + 4;
        let json_str = &resp[body_start..];
        let task: Task = serde_json::from_str(json_str).unwrap();
        assert_eq!(task.spec, "fazer deploy");
        assert_eq!(task.metadata.requested_by, "agent-bravo");

        // Recupera via GET
        let mut stream2 = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
        stream2
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let get_req = format!(
            "GET /a2a/tasks/{} HTTP/1.1\r\nHost: localhost\r\n\r\n",
            task.id
        );
        stream2.write_all(get_req.as_bytes()).unwrap();
        stream2.flush().unwrap();

        let mut buf2 = [0u8; 2048];
        let n2 = stream2.read(&mut buf2).unwrap();
        let resp2 = String::from_utf8_lossy(&buf2[..n2]);
        assert!(resp2.contains("200 OK"), "resp2: {}", resp2);
        assert!(resp2.contains("fazer deploy"), "resp2: {}", resp2);
    }

    #[test]
    fn servidor_adiciona_artifact() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let addr = format!("127.0.0.1:{}", port);
        let mgr_clone = Arc::clone(&manager);
        thread::spawn(move || {
            serve_a2a(&addr, mgr_clone).unwrap();
        });
        thread::sleep(Duration::from_millis(100));

        // Cria tarefa
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
        let body = r#"{"spec":"compilar","requested_by":"agent-charlie"}"#;
        let req = format!(
            "POST /a2a/tasks HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(req.as_bytes()).unwrap();
        let mut buf = [0u8; 2048];
        let n = stream.read(&mut buf).unwrap();
        let resp = String::from_utf8_lossy(&buf[..n]);
        let body_start = resp.find("\r\n\r\n").unwrap() + 4;
        let task: Task = serde_json::from_str(&resp[body_start..]).unwrap();

        // Adiciona artifact
        let mut stream2 = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
        let art_body =
            r#"{"content":"log de build","mime_type":"text/plain","checkpoint_id":"cp-42"}"#;
        let art_req = format!(
            "POST /a2a/tasks/{}/artifacts HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            task.id,
            art_body.len(),
            art_body
        );
        stream2.write_all(art_req.as_bytes()).unwrap();
        let mut buf2 = [0u8; 2048];
        let n2 = stream2.read(&mut buf2).unwrap();
        let resp2 = String::from_utf8_lossy(&buf2[..n2]);
        assert!(resp2.contains("200 OK"), "resp2: {}", resp2);
        assert!(resp2.contains("artifact added"), "resp2: {}", resp2);

        // Verifica que a tarefa possui o artifact
        let mgr = manager.lock().unwrap();
        let t = mgr.get_task(&task.id).unwrap();
        assert_eq!(t.artifacts.len(), 1);
        assert_eq!(t.artifacts[0].content, "log de build");
        assert_eq!(t.artifacts[0].checkpoint_id, Some("cp-42".to_string()));
    }

    #[test]
    fn servidor_retorna_404_para_rota_invalida() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let addr = format!("127.0.0.1:{}", port);
        let mgr_clone = Arc::clone(&manager);
        thread::spawn(move || {
            serve_a2a(&addr, mgr_clone).unwrap();
        });
        thread::sleep(Duration::from_millis(100));

        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let req = "GET /a2a/inexistente HTTP/1.1\r\nHost: localhost\r\n\r\n";
        stream.write_all(req.as_bytes()).unwrap();
        stream.flush().unwrap();

        let mut buf = [0u8; 512];
        let n = stream.read(&mut buf).unwrap();
        let resp = String::from_utf8_lossy(&buf[..n]);
        assert!(resp.contains("404 Not Found"), "resp: {}", resp);
    }
}
