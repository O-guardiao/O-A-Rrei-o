use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Tipos de hooks lifecycle suportados.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HookName {
    PreToolCall,
    PostToolCall,
    TransformToolResult,
    TransformTerminalOutput,
    PreLlmCall,
    PostLlmCall,
    PreApiRequest,
    PostApiRequest,
    TransformLlmOutput,
    OnSessionStart,
    OnSessionEnd,
    OnSessionReset,
    PreGatewayDispatch,
    PreApprovalRequest,
    PostApprovalResponse,
    SubagentStop,
}

impl HookName {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pre_tool_call" => Some(Self::PreToolCall),
            "post_tool_call" => Some(Self::PostToolCall),
            "transform_tool_result" => Some(Self::TransformToolResult),
            "transform_terminal_output" => Some(Self::TransformTerminalOutput),
            "pre_llm_call" => Some(Self::PreLlmCall),
            "post_llm_call" => Some(Self::PostLlmCall),
            "pre_api_request" => Some(Self::PreApiRequest),
            "post_api_request" => Some(Self::PostApiRequest),
            "transform_llm_output" => Some(Self::TransformLlmOutput),
            "on_session_start" => Some(Self::OnSessionStart),
            "on_session_end" => Some(Self::OnSessionEnd),
            "on_session_reset" => Some(Self::OnSessionReset),
            "pre_gateway_dispatch" => Some(Self::PreGatewayDispatch),
            "pre_approval_request" => Some(Self::PreApprovalRequest),
            "post_approval_response" => Some(Self::PostApprovalResponse),
            "subagent_stop" => Some(Self::SubagentStop),
            _ => None,
        }
    }
}

/// Callback de hook.
pub type HookCallback = Box<dyn Fn(&Value) -> Result<Option<Value>> + Send + Sync>;

/// Registro de hooks. Permite múltiplos callbacks por hook.
/// Thread-safe via RwLock — pode ser compartilhado entre threads e closures.
#[derive(Clone)]
pub struct HookRegistry {
    hooks: Arc<RwLock<HashMap<HookName, Vec<HookCallback>>>>,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self {
            hooks: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Registra um callback para um hook.
    pub fn register(&self, name: HookName, callback: HookCallback) {
        let mut hooks = self.hooks.write().unwrap();
        hooks.entry(name).or_default().push(callback);
    }

    /// Invoca todos os callbacks de um hook.
    /// Para `pre_tool_call`: primeiro bloqueio wins (pode abortar).
    /// Para `transform_tool_result`: o último resultado substitui.
    pub fn invoke(&self, name: &HookName, input: &Value) -> Result<Option<Value>> {
        let hooks = self.hooks.read().unwrap();
        let callbacks = match hooks.get(name) {
            Some(cbs) => cbs,
            None => return Ok(None),
        };

        let mut result: Option<Value> = None;
        for cb in callbacks {
            match cb(input) {
                Ok(Some(transformed)) => {
                    if *name == HookName::PreToolCall {
                        // PreToolCall: primeiro bloqueio wins
                        return Ok(Some(transformed));
                    }
                    result = Some(transformed);
                }
                Ok(None) => {
                    if *name == HookName::PreToolCall {
                        // PreToolCall retornou None = abort
                        return Ok(None);
                    }
                }
                Err(e) => {
                    if *name == HookName::PreToolCall {
                        return Err(e);
                    }
                    // Outros hooks: loga erro e continua
                    eprintln!("[hook] erro em {:?}: {}", name, e);
                }
            }
        }
        Ok(result)
    }

    /// Verifica se um hook tem callbacks registrados.
    pub fn has_hook(&self, name: &HookName) -> bool {
        let hooks = self.hooks.read().unwrap();
        hooks.get(name).map(|v| !v.is_empty()).unwrap_or(false)
    }

    /// Lista hooks registrados.
    pub fn registered_hooks(&self) -> Vec<HookName> {
        let hooks = self.hooks.read().unwrap();
        hooks.keys().cloned().collect()
    }
}

impl Default for HookRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_invoke() {
        let registry = HookRegistry::new();
        registry.register(
            HookName::PreLlmCall,
            Box::new(|input| {
                let mut out = input.clone();
                out["modified"] = serde_json::json!(true);
                Ok(Some(out))
            }),
        );

        let result = registry
            .invoke(&HookName::PreLlmCall, &serde_json::json!({"msg": "hello"}))
            .unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap()["modified"], true);
    }

    #[test]
    fn pre_tool_call_aborts() {
        let registry = HookRegistry::new();
        registry.register(
            HookName::PreToolCall,
            Box::new(|_input| {
                Ok(None) // abort
            }),
        );

        let result = registry
            .invoke(&HookName::PreToolCall, &serde_json::json!({"tool": "rm"}))
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn unknown_hook_returns_none() {
        let registry = HookRegistry::new();
        let result = registry
            .invoke(&HookName::PostLlmCall, &serde_json::json!({}))
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn transform_chain() {
        let registry = HookRegistry::new();
        registry.register(
            HookName::TransformToolResult,
            Box::new(|input| {
                let mut out = input.clone();
                out["step"] = serde_json::json!(1);
                Ok(Some(out))
            }),
        );
        registry.register(
            HookName::TransformToolResult,
            Box::new(|input| {
                let mut out = input.clone();
                out["step"] = serde_json::json!(2);
                Ok(Some(out))
            }),
        );

        let result = registry
            .invoke(&HookName::TransformToolResult, &serde_json::json!({}))
            .unwrap();
        assert_eq!(result.unwrap()["step"], 2);
    }
}
