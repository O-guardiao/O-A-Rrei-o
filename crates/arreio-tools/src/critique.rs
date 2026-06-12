//! VerifierCritiqueTool — o Verifier como critique agent (PVC-Q2.2).
//!
//! Expõe o `VerificationAgent` do arreio-actors como tool do ToolRegistry:
//! o Verifier é invocado pelo harness como ferramenta de crítica pontual,
//! nunca como orquestrador. A variante registrada usa `verify_fast`
//! (heurística determinística, sem LLM) — segura para pipelines críticos;
//! a análise adversarial completa com LLM permanece disponível via
//! `VerificationAgent::verify` para quem detém um ProviderClient.

use crate::{ToolHandler, ToolRequest, ToolResult};
use anyhow::Result;
use arreio_actors::VerificationAgent;
use arreio_provider::{ToolDescriptor, ToolFunction};

/// Tool de crítica determinística sobre código + spec.
pub struct VerifierCritiqueTool;

impl VerifierCritiqueTool {
    pub fn new() -> Self {
        Self
    }

    /// Descriptor para registro no ToolRegistry.
    pub fn descriptor() -> ToolDescriptor {
        ToolDescriptor {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "verifier_critique".to_string(),
                description: "Crítica adversarial determinística de código contra uma \
                              especificação: detecta unwraps excessivos, TODOs/FIXMEs e \
                              baixa cobertura da spec. Retorna bugs encontrados e confiança."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "code": {"type": "string", "description": "Código a criticar"},
                        "spec": {"type": "string", "description": "Especificação de referência"}
                    },
                    "required": ["code", "spec"]
                }),
            },
        }
    }
}

impl Default for VerifierCritiqueTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolHandler for VerifierCritiqueTool {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
        let code = request
            .arguments
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let spec = request
            .arguments
            .get("spec")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if code.is_empty() || spec.is_empty() {
            return Ok(ToolResult::err(
                "verifier_critique requer argumentos 'code' e 'spec'",
            ));
        }

        let result = VerificationAgent::verify_fast(code, spec);
        Ok(ToolResult::ok(serde_json::to_string(&result)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn critica_codigo_com_unwraps() {
        let tool = VerifierCritiqueTool::new();
        let result = tool
            .handle(ToolRequest {
                name: "verifier_critique".into(),
                arguments: serde_json::json!({
                    "code": "fn f() { a().unwrap(); b().unwrap(); c().unwrap(); d().unwrap(); }",
                    "spec": "função f robusta"
                }),
            })
            .unwrap();
        assert!(result.success);
        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["passed"], false);
    }

    #[test]
    fn aprova_codigo_que_cobre_spec() {
        let tool = VerifierCritiqueTool::new();
        let result = tool
            .handle(ToolRequest {
                name: "verifier_critique".into(),
                arguments: serde_json::json!({
                    "code": "fn calcular_soma(valores: &[i64]) -> i64 { valores.iter().sum() }",
                    "spec": "calcular_soma valores"
                }),
            })
            .unwrap();
        assert!(result.success);
        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["passed"], true);
    }

    #[test]
    fn exige_argumentos() {
        let tool = VerifierCritiqueTool::new();
        let result = tool
            .handle(ToolRequest {
                name: "verifier_critique".into(),
                arguments: serde_json::json!({"code": "fn f() {}"}),
            })
            .unwrap();
        assert!(!result.success);
    }

    #[test]
    fn descriptor_valido() {
        let d = VerifierCritiqueTool::descriptor();
        assert_eq!(d.function.name, "verifier_critique");
    }
}
