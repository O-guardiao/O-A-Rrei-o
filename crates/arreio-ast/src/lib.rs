use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

// ── Tipos de saída ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct FuncSig {
    pub name: String,
    pub params: Vec<String>,
    pub return_type: Option<String>,
    pub is_pub: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TypeSig {
    pub name: String,
    pub kind: String, // "struct" | "enum" | "trait"
    pub is_pub: bool,
}

/// Mapa compacto de símbolos de um arquivo.
/// Enviado ao Ator Desenvolvedor no lugar do arquivo completo.
/// Redução típica de 90% nos tokens.
#[derive(Debug, Serialize, Deserialize)]
pub struct SymbolMap {
    pub file: String,
    pub functions: Vec<FuncSig>,
    pub types: Vec<TypeSig>,
    pub imports: Vec<String>,
}

impl SymbolMap {
    /// Serializa em JSON compacto (sem espaços) para minimizar tokens.
    pub fn to_compact_json(&self) -> String {
        serde_json::to_string(self).expect("falha ao serializar SymbolMap")
    }
}

// ── Extrator Rust (via `syn`) ─────────────────────────────────────────────────

pub fn extract_from_file(path: &Path) -> Result<SymbolMap> {
    let src = fs::read_to_string(path).with_context(|| format!("lendo {}", path.display()))?;
    extract_from_str(&src, &path.to_string_lossy())
}

pub fn extract_from_str(src: &str, file_label: &str) -> Result<SymbolMap> {
    let ast =
        syn::parse_file(src).with_context(|| format!("syn parse falhou em {}", file_label))?;

    let mut functions = Vec::new();
    let mut types = Vec::new();
    let mut imports = Vec::new();

    for item in &ast.items {
        match item {
            syn::Item::Fn(f) => functions.push(fn_sig(f)),
            syn::Item::Impl(imp) => {
                for item in &imp.items {
                    if let syn::ImplItem::Fn(m) = item {
                        functions.push(impl_fn_sig(m, &imp.self_ty));
                    }
                }
            }
            syn::Item::Struct(s) => types.push(TypeSig {
                name: s.ident.to_string(),
                kind: "struct".to_string(),
                is_pub: is_public(&s.vis),
            }),
            syn::Item::Enum(e) => types.push(TypeSig {
                name: e.ident.to_string(),
                kind: "enum".to_string(),
                is_pub: is_public(&e.vis),
            }),
            syn::Item::Trait(t) => types.push(TypeSig {
                name: t.ident.to_string(),
                kind: "trait".to_string(),
                is_pub: is_public(&t.vis),
            }),
            syn::Item::Use(u) => {
                imports.push(quote::quote!(#u).to_string());
            }
            _ => {}
        }
    }

    Ok(SymbolMap {
        file: file_label.to_string(),
        functions,
        types,
        imports,
    })
}

fn fn_sig(f: &syn::ItemFn) -> FuncSig {
    FuncSig {
        name: f.sig.ident.to_string(),
        params: params_of(&f.sig.inputs),
        return_type: return_type_of(&f.sig.output),
        is_pub: is_public(&f.vis),
    }
}

fn impl_fn_sig(m: &syn::ImplItemFn, self_ty: &syn::Type) -> FuncSig {
    let prefix = quote::quote!(#self_ty).to_string().replace(' ', "");
    FuncSig {
        name: format!("{}::{}", prefix, m.sig.ident),
        params: params_of(&m.sig.inputs),
        return_type: return_type_of(&m.sig.output),
        is_pub: is_public(&m.vis),
    }
}

fn params_of(inputs: &syn::punctuated::Punctuated<syn::FnArg, syn::token::Comma>) -> Vec<String> {
    inputs
        .iter()
        .map(|arg| match arg {
            syn::FnArg::Receiver(_) => "self".to_string(),
            syn::FnArg::Typed(pt) => {
                let name = match pt.pat.as_ref() {
                    syn::Pat::Ident(pi) => pi.ident.to_string(),
                    _ => "_".to_string(),
                };
                let ty = quote::quote!(#(pt.ty)).to_string();
                format!("{}: {}", name, ty)
            }
        })
        .collect()
}

fn return_type_of(output: &syn::ReturnType) -> Option<String> {
    match output {
        syn::ReturnType::Default => None,
        syn::ReturnType::Type(_, ty) => Some(quote::quote!(#ty).to_string()),
    }
}

fn is_public(vis: &syn::Visibility) -> bool {
    matches!(vis, syn::Visibility::Public(_))
}

// ── Fallback: extração regex para linguagens não-Rust ────────────────────────

pub fn extract_generic(src: &str, file_label: &str) -> SymbolMap {
    use std::collections::HashSet;
    let mut functions = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for line in src.lines() {
        let trimmed = line.trim();
        // Captura: def foo(, function foo(, fn foo(, func foo(
        if let Some(name) = capture_fn_name(trimmed) {
            if seen.insert(name.clone()) {
                functions.push(FuncSig {
                    name,
                    params: vec![],
                    return_type: None,
                    is_pub: trimmed.starts_with("pub"),
                });
            }
        }
    }

    SymbolMap {
        file: file_label.to_string(),
        functions,
        types: vec![],
        imports: vec![],
    }
}

fn capture_fn_name(line: &str) -> Option<String> {
    let prefixes = [
        "def ",
        "function ",
        "fn ",
        "func ",
        "pub fn ",
        "pub function ",
    ];
    for prefix in prefixes {
        if let Some(rest) = line.strip_prefix(prefix) {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
//! Module-level documentation that takes up a lot of space.
//! This module provides Foo and Bar types for general purpose usage.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::path::PathBuf;

/// A struct that does many things. Contains extensive docs so that
/// when we strip it down to just signatures the JSON is smaller.
pub struct Foo {
    x: i32,
    label: String,
    data: HashMap<String, Vec<u8>>,
}

impl Foo {
    /// Creates a new Foo with the given x value.
    /// Allocates a new label with default prefix.
    pub fn new(x: i32) -> Self {
        let label = format!("foo_{}", x);
        Self { x, label, data: HashMap::new() }
    }

    /// Internal helper — do not call from outside.
    /// Returns the squared value of x for internal calculations.
    fn private_helper(&self) -> i32 {
        let sq = self.x * self.x;
        sq
    }

    /// Inserts a key-value pair into the internal data map.
    /// Key must be non-empty; value is a raw byte buffer.
    pub fn insert(&mut self, key: String, value: Vec<u8>) -> bool {
        if key.is_empty() { return false; }
        self.data.insert(key, value);
        true
    }
}

/// Top-level function with several parameters and a long body.
pub fn top_level(a: String, b: i32) -> bool {
    if a.is_empty() { return false; }
    if b < 0 { return false; }
    let result = a.len() as i32 + b;
    result > 0
}

/// Another exported function for processing paths.
pub fn process_path(path: PathBuf, recursive: bool) -> Option<String> {
    if !path.exists() { return None; }
    let s = path.to_string_lossy().to_string();
    Some(s)
}

/// Bar enum with several variants.
pub enum Bar {
    /// Variant A with no data
    A,
    /// Variant B with an integer payload
    B(i32),
    /// Variant C with named fields
    C { label: String, value: f64 },
}
"#;

    #[test]
    fn extracts_functions() {
        let map = extract_from_str(SAMPLE, "test.rs").unwrap();
        let names: Vec<_> = map.functions.iter().map(|f| &f.name).collect();
        assert!(names.iter().any(|n| n.contains("new")));
        assert!(names.iter().any(|n| n.contains("top_level")));
    }

    #[test]
    fn extracts_types() {
        let map = extract_from_str(SAMPLE, "test.rs").unwrap();
        let names: Vec<_> = map.types.iter().map(|t| &t.name).collect();
        assert!(names.contains(&&"Foo".to_string()));
        assert!(names.contains(&&"Bar".to_string()));
    }

    #[test]
    fn compact_json_is_smaller_than_source() {
        let map = extract_from_str(SAMPLE, "test.rs").unwrap();
        let json = map.to_compact_json();
        assert!(json.len() < SAMPLE.len());
    }

    #[test]
    fn generic_extractor_finds_python_style() {
        let py = "def add(a, b):\n    return a + b\ndef sub(a, b):\n    return a - b\n";
        let map = extract_generic(py, "utils.py");
        assert_eq!(map.functions.len(), 2);
    }
}
