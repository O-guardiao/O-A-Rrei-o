//! PromptMode — modos de raciocínio determinísticos (PVC-Q2.1).
//!
//! Traduz os padrões CoT/ToT/ReAct/PAL do mercado para a arquitetura O Arreio:
//! o modo é escolhido pelo harness (nunca pelo LLM) e cada modo define um
//! scaffold de prompt fixo e auditável. O LLM apenas preenche o scaffold;
//! o controle de fluxo permanece no `arreio-reasoning`/FSM.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Modo de prompting selecionado deterministicamente pelo harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptMode {
    /// Resposta direta, sem raciocínio explícito (default, menor custo).
    Direct,
    /// Chain-of-Thought: raciocínio passo a passo em uma única chamada.
    ChainOfThought,
    /// Tree-of-Thoughts harnessed: N ramos gerados em chamadas separadas;
    /// a seleção do ramo é feita pelo harness, nunca pelo LLM.
    TreeOfThoughts,
    /// ReAct harnessed: ciclos Thought→Action→Observation controlados pela
    /// FSM (estados explícitos), com budget por passo. O LLM nunca executa
    /// a ação — apenas a propõe; o harness executa e injeta a observação.
    ReActHarnessed,
    /// Program-Aided: o LLM gera um programa auditável; a execução é
    /// delegada ao hypervisor pelo chamador (nunca executada livremente).
    ProgramAided,
}

impl PromptMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            PromptMode::Direct => "direct",
            PromptMode::ChainOfThought => "chain_of_thought",
            PromptMode::TreeOfThoughts => "tree_of_thoughts",
            PromptMode::ReActHarnessed => "react_harnessed",
            PromptMode::ProgramAided => "program_aided",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "direct" => Some(Self::Direct),
            "chain_of_thought" | "cot" => Some(Self::ChainOfThought),
            "tree_of_thoughts" | "tot" => Some(Self::TreeOfThoughts),
            "react_harnessed" | "react" => Some(Self::ReActHarnessed),
            "program_aided" | "pal" => Some(Self::ProgramAided),
            _ => None,
        }
    }

    /// Scaffold determinístico anexado ao system prompt.
    /// Os marcadores (`ANSWER:`, `ACTION:`, etc.) são contratos de parsing
    /// do harness — mudá-los exige atualizar `arreio-reasoning`.
    pub fn system_scaffold(&self) -> &'static str {
        match self {
            PromptMode::Direct => {
                "Responda diretamente, sem explicar o raciocínio. \
                 Termine com a linha 'ANSWER: <resposta final>'."
            }
            PromptMode::ChainOfThought => {
                "Raciocine passo a passo, numerando cada passo ('Step 1:', 'Step 2:', ...). \
                 Termine com a linha 'ANSWER: <resposta final>'."
            }
            PromptMode::TreeOfThoughts => {
                "Proponha UMA linha de raciocínio completa para o problema. \
                 Primeiro avalie sua própria confiança na linha 'SCORE: <0.0-1.0>'. \
                 Depois desenvolva o raciocínio e termine com 'ANSWER: <resposta final>'."
            }
            PromptMode::ReActHarnessed => {
                "Você opera em ciclos Thought/Action controlados externamente. \
                 Responda SOMENTE com um dos dois formatos:\n\
                 1. 'THOUGHT: <análise>' seguido de 'ACTION: {\"tool\": \"<nome>\", \"args\": {...}}'\n\
                 2. 'THOUGHT: <análise>' seguido de 'FINAL: <resposta final>'\n\
                 Nunca execute a ação você mesmo; apenas a proponha. \
                 A observação do resultado será injetada no próximo turno."
            }
            PromptMode::ProgramAided => {
                "Resolva o problema escrevendo um programa. \
                 Retorne o programa dentro de um bloco ```program ... ```. \
                 O programa NÃO será executado automaticamente — ele será auditado \
                 e executado em sandbox pelo harness. \
                 Após o bloco, termine com 'ANSWER: PROGRAM_PENDING_EXECUTION'."
            }
        }
    }

    /// True se o modo exige múltiplas iterações controladas pelo harness.
    pub fn is_iterative(&self) -> bool {
        matches!(self, PromptMode::ReActHarnessed)
    }

    /// Número padrão de ramos para Tree-of-Thoughts.
    pub fn default_branches(&self) -> usize {
        match self {
            PromptMode::TreeOfThoughts => 3,
            _ => 1,
        }
    }
}

impl Default for PromptMode {
    fn default() -> Self {
        PromptMode::Direct
    }
}

impl fmt::Display for PromptMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_as_str_from_str() {
        for mode in [
            PromptMode::Direct,
            PromptMode::ChainOfThought,
            PromptMode::TreeOfThoughts,
            PromptMode::ReActHarnessed,
            PromptMode::ProgramAided,
        ] {
            assert_eq!(PromptMode::from_str(mode.as_str()), Some(mode));
        }
    }

    #[test]
    fn aliases_curtos_sao_aceitos() {
        assert_eq!(
            PromptMode::from_str("cot"),
            Some(PromptMode::ChainOfThought)
        );
        assert_eq!(
            PromptMode::from_str("react"),
            Some(PromptMode::ReActHarnessed)
        );
        assert_eq!(PromptMode::from_str("pal"), Some(PromptMode::ProgramAided));
        assert_eq!(PromptMode::from_str("tot"), Some(PromptMode::TreeOfThoughts));
        assert_eq!(PromptMode::from_str("invalido"), None);
    }

    #[test]
    fn apenas_react_e_iterativo() {
        assert!(PromptMode::ReActHarnessed.is_iterative());
        assert!(!PromptMode::Direct.is_iterative());
        assert!(!PromptMode::ChainOfThought.is_iterative());
        assert!(!PromptMode::TreeOfThoughts.is_iterative());
        assert!(!PromptMode::ProgramAided.is_iterative());
    }

    #[test]
    fn tot_tem_tres_ramos_por_padrao() {
        assert_eq!(PromptMode::TreeOfThoughts.default_branches(), 3);
        assert_eq!(PromptMode::Direct.default_branches(), 1);
    }

    #[test]
    fn scaffold_react_contem_marcadores_de_parsing() {
        let s = PromptMode::ReActHarnessed.system_scaffold();
        assert!(s.contains("THOUGHT:"));
        assert!(s.contains("ACTION:"));
        assert!(s.contains("FINAL:"));
    }

    #[test]
    fn serializa_como_json() {
        let json = serde_json::to_string(&PromptMode::ChainOfThought).unwrap();
        let de: PromptMode = serde_json::from_str(&json).unwrap();
        assert_eq!(de, PromptMode::ChainOfThought);
    }
}
