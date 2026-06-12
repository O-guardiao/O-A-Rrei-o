use anyhow::Result;
use arreio_kernel::Blackboard;

/// Extrai símbolos de um arquivo via LSP e publica no Blackboard.
pub struct SymbolFetcher {
    blackboard: Blackboard,
}

impl SymbolFetcher {
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }

    /// Publica símbolos de um arquivo no Blackboard como `lsp.symbols.{file_path}`.
    pub fn publish_symbols(
        &self,
        file_path: &str,
        symbols: &[crate::protocol::DocumentSymbol],
    ) -> Result<()> {
        let key = format!("symbols.{}", file_path.replace('/', "."));
        let value = serde_json::to_value(symbols)?;
        self.blackboard.put_tuple("lsp", &key, value)
    }

    /// Recupera símbolos publicados de um arquivo.
    pub fn get_symbols(&self, file_path: &str) -> Option<Vec<crate::protocol::DocumentSymbol>> {
        let key = format!("symbols.{}", file_path.replace('/', "."));
        self.blackboard
            .get_tuple("lsp", &key)
            .and_then(|v| serde_json::from_value(v).ok())
    }

    /// Outline simplificado: lista nomes e kinds dos símbolos.
    pub fn outline(symbols: &[crate::protocol::DocumentSymbol]) -> Vec<(String, String)> {
        let mut result = Vec::new();
        for sym in symbols {
            Self::collect_outline(sym, &mut result);
        }
        result
    }

    fn collect_outline(sym: &crate::protocol::DocumentSymbol, result: &mut Vec<(String, String)>) {
        let kind_str = format!("{:?}", sym.kind);
        result.push((sym.name.clone(), kind_str));
        if let Some(children) = &sym.children {
            for child in children {
                Self::collect_outline(child, result);
            }
        }
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{DocumentSymbol, SymbolKind};
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_fetcher() -> SymbolFetcher {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        let bb = Blackboard::open(&p).unwrap();
        SymbolFetcher::new(bb)
    }

    #[test]
    fn publish_and_get_symbols() {
        let fetcher = temp_fetcher();
        let symbols = vec![DocumentSymbol {
            name: "main".to_string(),
            detail: None,
            kind: SymbolKind::Function,
            children: None,
        }];
        fetcher.publish_symbols("src/main.rs", &symbols).unwrap();
        let retrieved = fetcher.get_symbols("src/main.rs").unwrap();
        assert_eq!(retrieved.len(), 1);
        assert_eq!(retrieved[0].name, "main");
    }

    #[test]
    fn outline_flattened() {
        let symbols = vec![DocumentSymbol {
            name: "MyStruct".to_string(),
            detail: None,
            kind: SymbolKind::Struct,
            children: Some(vec![DocumentSymbol {
                name: "field1".to_string(),
                detail: None,
                kind: SymbolKind::Field,
                children: None,
            }]),
        }];
        let outline = SymbolFetcher::outline(&symbols);
        assert_eq!(outline.len(), 2);
        assert_eq!(outline[0].0, "MyStruct");
        assert_eq!(outline[1].0, "field1");
    }
}
