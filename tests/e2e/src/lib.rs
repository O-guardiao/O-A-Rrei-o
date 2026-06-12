//! Helpers para testes end-to-end do Arreio.
//!
//! Estes testes executam o binário `arreio` em um diretório temporário,
//! usando providers mockados via variáveis de ambiente ou fixtures.

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Workspace temporário para testes E2E.
pub struct ArreioWorkspace {
    pub dir: TempDir,
    pub spec_path: PathBuf,
    pub blackboard_path: PathBuf,
}

impl ArreioWorkspace {
    /// Cria um workspace temporário com um diretório `.arreio/` vazio.
    pub fn new() -> Self {
        let dir = TempDir::new().expect("criar temp dir");
        let arreio_dir = dir.path().join(".arreio");
        std::fs::create_dir_all(&arreio_dir).expect("criar .arreio");
        Self {
            dir,
            spec_path: PathBuf::new(),
            blackboard_path: arreio_dir.join("blackboard.json"),
        }
    }

    /// Cria um arquivo `.spec` no workspace e retorna seu path.
    pub fn write_spec(&mut self, name: &str, content: &str) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::write(&path, content).expect("escrever spec");
        self.spec_path = path.clone();
        path
    }

    /// Cria um arquivo arbitrário no workspace (útil para skills, código-fonte, etc.).
    pub fn write_file(&self, rel_path: &str, content: &str) -> PathBuf {
        let path = self.dir.path().join(rel_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("criar diretório pai");
        }
        std::fs::write(&path, content).expect("escrever arquivo");
        path
    }

    /// Retorna um comando `arreio` configurado para rodar no workspace temporário.
    pub fn arreio_cmd(&self) -> Command {
        let mut cmd = Command::cargo_bin("arreio").expect("compilar binário arreio");
        cmd.current_dir(self.dir.path());
        // Desabilita qualquer provider cloud — testes usam MockProvider via fixtures
        cmd.env_remove("ANTHROPIC_API_KEY");
        cmd.env_remove("OPENAI_API_KEY");
        cmd.env_remove("GOOGLE_API_KEY");
        cmd.env_remove("DEEPSEEK_API_KEY");
        cmd.env_remove("AZURE_OPENAI_API_KEY");
        cmd.env_remove("AZURE_API_KEY");
        cmd
    }

    /// Carrega o DAG persistido no Blackboard do workspace.
    pub fn load_dag(&self) -> arreio_dag::Dag {
        let bb = arreio_kernel::Blackboard::open(&self.blackboard_path).expect("abrir blackboard");
        arreio_dag::Dag::load(bb).expect("carregar DAG")
    }

    /// Caminho raiz do workspace.
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// Fixture: spec simples que apenas cria um arquivo e valida com `echo ok`.
pub fn hello_world_spec() -> &'static str {
    r#"# Teste E2E: Hello World

Goal: criar arquivo hello.rs com fn main
Non-goals: complexidade
Constraints: nenhuma

## Milestones

### M1: Criar hello.rs
- Criar arquivo `hello.rs` com `fn main() { println!("hello"); }`
- Validation: `echo ok`
"#
}

/// Fixture: spec para testar ContextCollapser com threshold baixo.
pub fn context_collapse_spec() -> &'static str {
    r#"# Teste E2E: Context Collapse

Goal: criar arquivo dummy.txt
Non-goals: -
Constraints: -

## Milestones

### M1: Criar dummy
- Criar arquivo `dummy.txt` com conteúdo "ok"
- Validation: `echo ok`
"#
}
