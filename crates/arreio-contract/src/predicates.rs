use serde_json::Value;

/// Verifica se o valor é uma string não vazia ou um array/objeto não vazio.
pub fn non_empty(input: &Value) -> bool {
    match input {
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Null => false,
        Value::Bool(_) => true,
        Value::Number(_) => true,
    }
}

/// Verifica se o valor numérico está dentro do intervalo [min, max].
/// Para strings, verifica o comprimento. Para arrays/objetos, verifica o tamanho.
pub fn in_range(input: &Value, min: f64, max: f64) -> bool {
    match input {
        Value::Number(n) => {
            if let Some(v) = n.as_f64() {
                v >= min && v <= max
            } else {
                false
            }
        }
        Value::String(s) => {
            let len = s.len() as f64;
            len >= min && len <= max
        }
        Value::Array(a) => {
            let len = a.len() as f64;
            len >= min && len <= max
        }
        Value::Object(o) => {
            let len = o.len() as f64;
            len >= min && len <= max
        }
        _ => false,
    }
}

/// Verifica se uma string corresponde a um padrão regex.
pub fn matches_pattern(input: &Value, pattern: &str) -> bool {
    match input {
        Value::String(s) => {
            if let Ok(re) = regex::Regex::new(pattern) {
                re.is_match(s)
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Verifica se o valor é um JSON válido (sempre true para Value).
/// Stub intencional: como o input já é um serde_json::Value, ele é
/// por definição um JSON válido. A validação real seria contra schema.
pub fn is_valid_json(_input: &Value) -> bool {
    true
}

/// Verifica se o comprimento de string/array/objeto não excede o máximo.
pub fn max_length(input: &Value, max: usize) -> bool {
    match input {
        Value::String(s) => s.len() <= max,
        Value::Array(a) => a.len() <= max,
        Value::Object(o) => o.len() <= max,
        _ => true,
    }
}
