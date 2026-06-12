use std::path::{Path, PathBuf};

/// Camada de contexto hierárquico (padrão Codex).
#[derive(Debug, Clone)]
pub struct ContextLayer {
    pub source: String, // ex: "global", "project", "directory", "skills", "memory"
    pub content: String,
}

/// Contexto montado hierarquicamente.
#[derive(Debug, Clone)]
pub struct AssembledContext {
    pub layers: Vec<ContextLayer>,
}

impl AssembledContext {
    /// Concatena todas as camadas em um único texto, com separadores.
    pub fn to_prompt_section(&self) -> String {
        if self.layers.is_empty() {
            return String::new();
        }
        let mut out = "## Contexto Hierárquico\n\n".to_string();
        for layer in &self.layers {
            if !layer.content.trim().is_empty() {
                out.push_str(&format!(
                    "--- {} ---\n{}\n\n",
                    layer.source,
                    layer.content.trim()
                ));
            }
        }
        out
    }
}

/// Assembler de contexto no estilo Codex + hierarquia ARREIO.md (GAP-014).
/// Ordem: Managed → User → Project → Local → Directory → Skills → Memory → Task
pub struct ContextAssembler;

impl ContextAssembler {
    pub fn new() -> Self {
        Self
    }

    /// Monta o contexto completo para uma tarefa.
    pub fn assemble(
        &self,
        task_query: &str,
        work_dir: &Path,
        file_target: Option<&str>,
        skills_context: &str,
        memory_frame: Option<&str>,
    ) -> AssembledContext {
        let mut layers = Vec::new();

        // 1. Managed Policy (/etc/arreio/ARREIO.md ou AGENTS.md)
        if let Some(content) = Self::load_managed_arreio_md() {
            layers.push(ContextLayer {
                source: "managed".into(),
                content,
            });
        }

        // 2. Managed Drop-ins (/etc/arreio/settings.d/*.md)
        for (name, content) in Self::load_dropins(Path::new("/etc/arreio/settings.d")) {
            layers.push(ContextLayer {
                source: format!("managed-dropin:{}", name),
                content,
            });
        }

        // 3. User Memory (~/.arreio/ARREIO.md ou AGENTS.md)
        if let Some(content) = Self::load_user_arreio_md() {
            layers.push(ContextLayer {
                source: "user".into(),
                content,
            });
        }

        // 4. User Rules (~/.arreio/rules/*.md)
        for (name, content) in Self::load_dropins(&Self::arreio_home().join("rules")) {
            layers.push(ContextLayer {
                source: format!("user-rule:{}", name),
                content,
            });
        }

        // 5. Project Memory (./.arreio/ARREIO.md ou AGENTS.md)
        if let Some(content) = Self::load_project_arreio_md(work_dir) {
            layers.push(ContextLayer {
                source: "project".into(),
                content,
            });
        }

        // 6. Project Rules (./.arreio/rules/*.md)
        for (name, content) in Self::load_dropins(&work_dir.join(".arreio").join("rules")) {
            layers.push(ContextLayer {
                source: format!("project-rule:{}", name),
                content,
            });
        }

        // 7. Local Project (./ARREIO.local.md)
        if let Some(content) = Self::load_local_arreio_md(work_dir) {
            layers.push(ContextLayer {
                source: "local".into(),
                content,
            });
        }

        // 8. Directory (ARREIO.md ou AGENTS.md mais próximo do file_target)
        if let Some(target) = file_target {
            if let Some(content) = Self::load_directory_arreio_md(work_dir, target) {
                layers.push(ContextLayer {
                    source: "directory".into(),
                    content,
                });
            }
        }

        // 9. Skills
        if !skills_context.is_empty() {
            layers.push(ContextLayer {
                source: "skills".into(),
                content: skills_context.into(),
            });
        }

        // 10. Memory
        if let Some(mem) = memory_frame {
            if !mem.is_empty() {
                layers.push(ContextLayer {
                    source: "memory".into(),
                    content: mem.into(),
                });
            }
        }

        // 11. Task (a query em si)
        layers.push(ContextLayer {
            source: "task".into(),
            content: task_query.into(),
        });

        AssembledContext { layers }
    }

    fn arreio_home() -> PathBuf {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default();
        PathBuf::from(home).join(".arreio")
    }

    fn read_md(path: &Path) -> Option<String> {
        std::fs::read_to_string(path)
            .ok()
            .filter(|s| !s.trim().is_empty())
    }

    fn load_dropins(dir: &Path) -> Vec<(String, String)> {
        let mut results = Vec::new();
        if !dir.exists() {
            return results;
        }
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .map(|ext| ext == "md")
                    .unwrap_or(false)
            })
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if let Some(content) = Self::read_md(&entry.path()) {
                let name = entry.file_name().to_string_lossy().to_string();
                results.push((name, content));
            }
        }
        results
    }

    fn load_managed_arreio_md() -> Option<String> {
        Self::read_md(Path::new("/etc/arreio/ARREIO.md"))
            .or_else(|| Self::read_md(Path::new("/etc/arreio/AGENTS.md")))
    }

    fn load_user_arreio_md() -> Option<String> {
        let home = Self::arreio_home();
        Self::read_md(&home.join("ARREIO.md"))
            .or_else(|| Self::read_md(&home.join("AGENTS.md")))
    }

    fn load_project_arreio_md(work_dir: &Path) -> Option<String> {
        Self::read_md(&work_dir.join(".arreio").join("ARREIO.md"))
            .or_else(|| Self::read_md(&work_dir.join(".arreio").join("AGENTS.md")))
    }

    fn load_local_arreio_md(work_dir: &Path) -> Option<String> {
        Self::read_md(&work_dir.join("ARREIO.local.md"))
            .or_else(|| Self::read_md(&work_dir.join("AGENTS.local.md")))
    }

    fn load_directory_arreio_md(work_dir: &Path, file_target: &str) -> Option<String> {
        let target_path = work_dir.join(file_target);
        let mut dir = target_path.parent().unwrap_or(work_dir);

        while dir.starts_with(work_dir) {
            if let Some(content) = Self::read_md(&dir.join("ARREIO.md"))
                .or_else(|| Self::read_md(&dir.join("AGENTS.md")))
            {
                return Some(content);
            }
            if dir == work_dir {
                break;
            }
            dir = dir.parent().unwrap_or(work_dir);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn context_layering_order() {
        let dir = TempDir::new().unwrap();
        let work = dir.path();

        // Cria AGENTS.md no projeto
        std::fs::create_dir_all(work.join(".arreio")).unwrap();
        std::fs::write(
            work.join(".arreio/AGENTS.md"),
            "# Projeto\nRegras do projeto.",
        )
        .unwrap();

        // Cria AGENTS.md em subdiretório
        std::fs::create_dir_all(work.join("src/auth")).unwrap();
        std::fs::write(work.join("src/auth/AGENTS.md"), "# Auth\nRegras de auth.").unwrap();

        let assembler = ContextAssembler::new();
        let ctx = assembler.assemble(
            "implementar login",
            work,
            Some("src/auth/login.rs"),
            "skills context",
            Some("memory frame"),
        );

        let sources: Vec<_> = ctx.layers.iter().map(|l| l.source.as_str()).collect();
        assert!(sources.contains(&"project"));
        assert!(sources.contains(&"directory"));
        assert!(sources.contains(&"skills"));
        assert!(sources.contains(&"memory"));
        assert!(sources.contains(&"task"));

        // Directory deve ser o de src/auth
        let dir_layer = ctx.layers.iter().find(|l| l.source == "directory").unwrap();
        assert!(dir_layer.content.contains("Regras de auth"));
    }

    #[test]
    fn agents_md_injected() {
        let dir = TempDir::new().unwrap();
        let work = dir.path();
        std::fs::create_dir_all(work.join(".arreio")).unwrap();
        std::fs::write(work.join(".arreio/AGENTS.md"), "Use Rust.").unwrap();

        let assembler = ContextAssembler::new();
        let ctx = assembler.assemble("tarefa", work, None, "", None);
        let prompt = ctx.to_prompt_section();
        assert!(prompt.contains("Use Rust."));
    }

    #[test]
    fn nearest_directory_arreio_md() {
        let dir = TempDir::new().unwrap();
        let work = dir.path();
        std::fs::create_dir_all(work.join("src/auth")).unwrap();
        std::fs::write(work.join("src/auth/AGENTS.md"), "auth rules").unwrap();

        let found = ContextAssembler::load_directory_arreio_md(work, "src/auth/login.rs");
        assert_eq!(found.unwrap().trim(), "auth rules");
    }

    #[test]
    fn arreio_md_hierarchy_gap_014() {
        let dir = TempDir::new().unwrap();
        let work = dir.path();

        // Project ARREIO.md
        std::fs::create_dir_all(work.join(".arreio")).unwrap();
        std::fs::write(work.join(".arreio/ARREIO.md"), "# Project\nProjeto rules.").unwrap();

        // Project rules drop-in
        std::fs::create_dir_all(work.join(".arreio/rules")).unwrap();
        std::fs::write(work.join(".arreio/rules/backend.md"), "# Backend\nUse async.").unwrap();

        // Local ARREIO
        std::fs::write(work.join("ARREIO.local.md"), "# Local\nLocal override.").unwrap();

        // Directory ARREIO.md
        std::fs::create_dir_all(work.join("src/auth")).unwrap();
        std::fs::write(work.join("src/auth/ARREIO.md"), "# Auth\nAuth rules.").unwrap();

        let assembler = ContextAssembler::new();
        let ctx = assembler.assemble(
            "implementar login",
            work,
            Some("src/auth/login.rs"),
            "",
            None,
        );

        let sources: Vec<_> = ctx.layers.iter().map(|l| l.source.as_str()).collect();
        assert!(sources.contains(&"project"), "deve conter project");
        assert!(sources.contains(&"project-rule:backend.md"), "deve conter project-rule");
        assert!(sources.contains(&"local"), "deve conter local");
        assert!(sources.contains(&"directory"), "deve conter directory");
        assert!(sources.contains(&"task"), "deve conter task");

        // Verifica conteúdo
        let local_layer = ctx.layers.iter().find(|l| l.source == "local").unwrap();
        assert!(local_layer.content.contains("Local override"));

        let rule_layer = ctx.layers.iter().find(|l| l.source == "project-rule:backend.md").unwrap();
        assert!(rule_layer.content.contains("Use async"));
    }
}
