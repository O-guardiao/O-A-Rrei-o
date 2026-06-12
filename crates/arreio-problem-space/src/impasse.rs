use serde::{Deserialize, Serialize};

use crate::operator::Operator;
use crate::problem_space::State;

/// Tipos de impasse segundo a teoria do Espaço de Problemas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImpasseType {
    /// Nenhum operador é aplicável ao estado atual.
    StateNoChange,
    /// Dois ou mais operadores possuem a mesma preferência.
    OperatorTie,
    /// Operadores possuem precondições mutuamente excludentes.
    OperatorConflict,
    /// Todos os operadores aplicáveis falharam na execução.
    Rejection,
}

/// Representa um impasse detectado no espaço de problemas.
#[derive(Debug, Clone, PartialEq)]
pub struct Impasse {
    pub impasse_type: ImpasseType,
    pub state: State,
    pub candidates: Vec<Operator>,
}
