use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

// -- Plugin Hook Engine -------------------------------------------------------

/// Engine que registra callbacks reais para hooks declarados por plugins.
pub struct PluginHookEngine;

impl PluginHookEngine {
    /// Registra hooks reais no HookRegistry baseado no manifesto do plugin.
    pub fn register_plugin_hooks(
        registry: &crate::hooks::HookRegistry,
        plugin: &DiscoveredPlugin,
    ) {
        for hook_str in &plugin.manifest.provides_hooks {
            if let Some(hook_name) = crate::hooks::HookName::from_str(hook_str) {
                let plugin_name = plugin.manifest.name.clone();
                let hook_str = hook_str.clone();
                registry.register(
                    hook_name,
                    Box::new(move |value| {
                        let mut out = value.clone();
                        out["_plugin_source"] = Value::String(plugin_name.clone());
                        out["_hook_name"] = Value::String(hook_str.clone());
                        eprintln!("[plugin-hook] [{}] {} invocado", plugin_name, hook_str);
                        Ok(Some(out))
                    }),
                );
            }
        }
    }
}

/// Retorna o diretorio onde plugins bundled podem estar (ao lado do binario).
fn bundled_plugins_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.join("plugins")))
}

/// Manifesto de um plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub kind: PluginKind,
    pub requires_env: Vec<String>,
    pub provides_tools: Vec<String>,
    pub provides_hooks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    Standalone,
    Backend,
    Platform,
    ModelProvider,
}

/// Plugin descoberto no filesystem.
#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    pub manifest: PluginManifest,
    pub path: PathBuf,
    pub source: PluginSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginSource {
    Bundled,
    User,
    Project,
}

/// Discovery de plugins em multiplas fontes.
pub struct PluginDiscovery;

impl PluginDiscovery {
    pub fn discover(arreio_home: &Path, project_dir: &Path) -> Vec<DiscoveredPlugin> {
        let mut plugins = Vec::new();

        // Bundled: plugins/ no diretorio do binario
        if let Some(bundled_dir) = bundled_plugins_dir() {
            if bundled_dir.exists() {
                plugins.extend(Self::scan_dir(&bundled_dir, PluginSource::Bundled));
            }
        }

        // User: ~/.arreio/plugins/
        let user_plugins = arreio_home.join("plugins");
        if user_plugins.exists() {
            plugins.extend(Self::scan_dir(&user_plugins, PluginSource::User));
        }

        // Project: ./.arreio/plugins/
        let project_plugins = project_dir.join(".arreio/plugins");
        if project_plugins.exists() {
            plugins.extend(Self::scan_dir(&project_plugins, PluginSource::Project));
        }

        plugins
    }

    fn scan_dir(dir: &Path, source: PluginSource) -> Vec<DiscoveredPlugin> {
        let mut results = Vec::new();
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    results.extend(Self::scan_plugin_dir(&path, source.clone()));
                } else if path.extension().map(|e| e == "yaml").unwrap_or(false) {
                    if let Some(plugin) = Self::load_manifest(&path, source.clone()) {
                        results.push(plugin);
                    }
                }
            }
        }
        results
    }

    fn scan_plugin_dir(dir: &Path, source: PluginSource) -> Vec<DiscoveredPlugin> {
        let mut results = Vec::new();
        let manifest_path = dir.join("plugin.yaml");
        if manifest_path.exists() {
            if let Some(plugin) = Self::load_manifest(&manifest_path, source.clone()) {
                results.push(plugin);
            }
        }
        // Layout flat: plugin.yaml diretamente no dir
        // Layout category: dir/subdir/plugin.yaml
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let manifest_path = path.join("plugin.yaml");
                    if manifest_path.exists() {
                        if let Some(plugin) = Self::load_manifest(&manifest_path, source.clone()) {
                            results.push(plugin);
                        }
                    }
                }
            }
        }
        results
    }

    fn load_manifest(path: &Path, source: PluginSource) -> Option<DiscoveredPlugin> {
        let content = fs::read_to_string(path).ok()?;
        let manifest: PluginManifest = serde_yaml::from_str(&content).ok()?;
        Some(DiscoveredPlugin {
            manifest,
            path: path.parent()?.to_path_buf(),
            source,
        })
    }
}

// -- Plugin Tool Handler ------------------------------------------------------

/// Handler generico para tools providas por plugins.
/// Executa um script no diretorio do plugin via Hypervisor.
pub struct PluginToolHandler {
    pub script_path: PathBuf,
    pub safe_root: PathBuf,
    pub timeout_secs: u64,
}

impl arreio_tools::ToolHandler for PluginToolHandler {
    fn handle(&self, request: arreio_tools::ToolRequest) -> anyhow::Result<arreio_tools::ToolResult> {
        let args = request
            .arguments
            .get("args")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let cmd = format!("{} {}", self.script_path.display(), args);
        let hypervisor = arreio_hypervisor::Hypervisor::new(self.timeout_secs);
        match hypervisor.run(&cmd, Some(&self.safe_root)) {
            Ok(result) => {
                let output = format!(
                    "exit_code: {}\nstdout:\n{}\nstderr:\n{}",
                    result.exit_code, result.stdout, result.stderr
                );
                if result.exit_code == 0 {
                    Ok(arreio_tools::ToolResult::ok(output))
                } else {
                    Ok(arreio_tools::ToolResult::err(output))
                }
            }
            Err(e) => Ok(arreio_tools::ToolResult::err(format!("plugin exec error: {}", e))),
        }
    }
}

/// Tenta encontrar um script executavel para a tool no diretorio do plugin.
/// Convencao: `{tool_name}.sh` (Unix) ou `{tool_name}.ps1/.bat/.cmd` (Windows).
pub fn find_plugin_script(plugin_dir: &Path, tool_name: &str) -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let exts = ["ps1", "bat", "cmd"];
    #[cfg(not(target_os = "windows"))]
    let exts = ["sh"];

    for ext in &exts {
        let candidate = plugin_dir.join(format!("{}.{}", tool_name, ext));
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Gera um ToolDescriptor para uma tool declarada por plugin.
pub fn build_plugin_tool_descriptor(name: &str, description: &str) -> arreio_provider::ToolDescriptor {
    arreio_provider::ToolDescriptor {
        r#type: "function".to_string(),
        function: arreio_provider::ToolFunction {
            name: name.to_string(),
            description: description.to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "args": {
                        "type": "string",
                        "description": "Argumentos para o script do plugin"
                    }
                },
                "required": []
            }),
        },
    }
}

/// Registra todas as tools declaradas por um plugin no ToolRegistry.
pub fn register_plugin_tools(
    registry: &arreio_tools::ToolRegistry,
    plugin: &DiscoveredPlugin,
    safe_root: &Path,
    timeout_secs: u64,
) {
    for tool_name in &plugin.manifest.provides_tools {
        if let Some(script_path) = find_plugin_script(&plugin.path, tool_name) {
            let desc = build_plugin_tool_descriptor(
                tool_name,
                &format!(
                    "Tool provida pelo plugin '{}' via script '{}'",
                    plugin.manifest.name,
                    script_path.display()
                ),
            );
            let handler = Arc::new(PluginToolHandler {
                script_path,
                safe_root: safe_root.to_path_buf(),
                timeout_secs,
            });
            registry.register(desc, handler);
        } else {
            eprintln!(
                "[arreio] AVISO: Plugin '{}' declara tool '{}' mas nenhum script foi encontrado em {}",
                plugin.manifest.name,
                tool_name,
                plugin.path.display()
            );
        }
    }
}

// -- Testes -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn discover_user_plugins() {
        let tmp = TempDir::new().unwrap();
        let plugins_dir = tmp.path().join("plugins");
        fs::create_dir_all(&plugins_dir).unwrap();
        fs::write(
            plugins_dir.join("plugin.yaml"),
            r#"name: test-plugin
version: "1.0.0"
description: A test plugin
kind: standalone
requires_env: []
provides_tools: [test_tool]
provides_hooks: []
"#,
        )
        .unwrap();

        let found = PluginDiscovery::discover(tmp.path(), tmp.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].manifest.name, "test-plugin");
        assert_eq!(found[0].source, PluginSource::User);
    }

    #[test]
    fn discover_category_layout() {
        let tmp = TempDir::new().unwrap();
        let cat_dir = tmp.path().join("plugins/backend");
        fs::create_dir_all(&cat_dir).unwrap();
        fs::write(
            cat_dir.join("plugin.yaml"),
            r#"name: backend-plugin
version: "1.0.0"
description: Backend plugin
kind: backend
requires_env: []
provides_tools: []
provides_hooks: [pre_llm_call]
"#,
        )
        .unwrap();

        let found = PluginDiscovery::discover(tmp.path(), tmp.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].manifest.name, "backend-plugin");
        assert_eq!(found[0].manifest.kind, PluginKind::Backend);
    }

    #[test]
    fn empty_discovery() {
        let tmp = TempDir::new().unwrap();
        let found = PluginDiscovery::discover(tmp.path(), tmp.path());
        assert!(found.is_empty());
    }
}
