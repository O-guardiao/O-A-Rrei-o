use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Descrição de uma tool para o LLM (formato compatível OpenAI/Ollama).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub r#type: String,
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// Chamada de tool retornada pelo LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub r#type: String,
    pub function: ToolCallFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: String,
}

/// Mensagem individual para requisições multi-turn.
#[derive(Debug, Clone)]
pub struct ChatMessageRequest {
    pub role: String,
    pub content: String,
    /// Conteúdo de raciocínio (reasoning/thinking) para modelos que exigem
    /// preservação entre turnos (ex: DeepSeek V4). `None` para provedores
    /// que não usam thinking mode ou não exigem eco do reasoning.
    pub reasoning_content: Option<String>,
}

/// Requisição simples e stateless para qualquer provedor LLM.
/// Suporta modo legacy (system + user únicos) ou modo multi-turn (messages).
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub system: String,
    pub user: String,
    /// Histórico de mensagens multi-turn. Se vazio, usa system+user legacy.
    pub messages: Vec<ChatMessageRequest>,
    pub tools: Option<Vec<ToolDescriptor>>,
}

impl ChatRequest {
    /// Cria requisição no modo legacy (system + user únicos).
    pub fn new(
        model: impl Into<String>,
        system: impl Into<String>,
        user: impl Into<String>,
    ) -> Self {
        Self {
            model: model.into(),
            system: system.into(),
            user: user.into(),
            messages: Vec::new(),
            tools: None,
        }
    }

    /// Cria requisição multi-turn a partir de histórico.
    pub fn with_messages(
        model: impl Into<String>,
        system: impl Into<String>,
        messages: Vec<ChatMessageRequest>,
    ) -> Self {
        Self {
            model: model.into(),
            system: system.into(),
            user: String::new(),
            messages,
            tools: None,
        }
    }

    /// Adiciona tools à requisição.
    pub fn with_tools(mut self, tools: Vec<ToolDescriptor>) -> Self {
        self.tools = Some(tools);
        self
    }
}

/// Resposta enxuta — texto gerado, tool calls e metadados de uso.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: String,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub rate_limit: Option<crate::rate_guard::RateLimitSnapshot>,
    /// Raciocínio/thinking extraído da resposta (DeepSeek V4, o1, etc.).
    /// Deve ser preservado e reenviado em turnos subsequentes quando o
    /// modelo exige eco do reasoning_content.
    pub reasoning_content: Option<String>,
}

/// Abstração unificada sobre provedores LLM.
/// Toda implementação é síncrona (sem async).
pub trait ProviderClient: Send + Sync {
    /// Envia uma requisição chat e retorna o texto gerado.
    fn chat(&self, req: ChatRequest) -> Result<ChatResponse>;

    /// Envia uma requisição chat em modo streaming.
    /// Retorna um iterador que produz chunks de texto conforme o LLM gera.
    fn chat_stream(
        &self,
        req: ChatRequest,
    ) -> Result<Box<dyn Iterator<Item = Result<String>> + Send>>;

    /// Nome legível do provedor (para métricas e logs).
    fn name(&self) -> &'static str;

    /// Clona o provider para uso em múltiplos contextos.
    fn clone_box(&self) -> Box<dyn ProviderClient>;

    /// Estima custo em USD para input/output tokens.
    fn cost_estimate(&self, input_tokens: u32, output_tokens: u32) -> f64;

    /// Gera embeddings para textos.
    fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>>;
}

impl ProviderClient for Box<dyn ProviderClient> {
    fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        (**self).chat(req)
    }
    fn chat_stream(
        &self,
        req: ChatRequest,
    ) -> Result<Box<dyn Iterator<Item = Result<String>> + Send>> {
        (**self).chat_stream(req)
    }
    fn name(&self) -> &'static str {
        (**self).name()
    }
    fn clone_box(&self) -> Box<dyn ProviderClient> {
        (**self).clone_box()
    }
    fn cost_estimate(&self, input_tokens: u32, output_tokens: u32) -> f64 {
        (**self).cost_estimate(input_tokens, output_tokens)
    }
    fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        (**self).embed(texts)
    }
}
