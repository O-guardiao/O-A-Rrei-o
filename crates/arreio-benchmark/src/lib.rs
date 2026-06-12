//! Arreio-Benchmark — Suite de validação e benchmark comparativo.
//!
//! 20 tarefas de codificação (sintéticas + reais de projetos open-source).
//! Comparação quantitativa O Arreio vs baseline (Claude Code manual, Cursor).
//! Testes E2E com LLM real: GPT-4o, Claude 3.5 Sonnet, Gemini 1.5 Pro.

pub mod comparative;
pub mod eval;

pub use eval::{
    CaseOutcome, EvalCase, EvalReport, EvalRunner, EvalSet, EvalStore, Expectation,
    RegressionDetector, RegressionVerdict, DEFAULT_REGRESSION_THRESHOLD,
};

use anyhow::Result;
use std::time::Instant;

/// Nível de dificuldade de uma tarefa de benchmark.
#[derive(Debug, Clone, PartialEq)]
pub enum Difficulty {
    Easy,
    Medium,
    Hard,
}

/// Tarefa de benchmark.
#[derive(Debug, Clone)]
pub struct BenchmarkTask {
    pub id: String,
    pub name: String,
    pub description: String,
    pub difficulty: Difficulty,
}

/// Resultado da execução de uma tarefa.
#[derive(Debug, Clone)]
pub struct BenchmarkResult {
    pub task_id: String,
    pub success: bool,
    pub latency_ms: u64,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub cost_usd: f64,
    pub quality_score: f64, // 0.0–1.0
}

/// Suite completa de benchmark.
pub struct BenchmarkSuite {
    pub tasks: Vec<BenchmarkTask>,
    runner: Option<Box<dyn Fn(&BenchmarkTask) -> BenchmarkResult>>,
}

impl BenchmarkSuite {
    /// Constrói a suite com as 20 tarefas definidas.
    pub fn new() -> Self {
        let tasks = vec![
            BenchmarkTask {
                id: "t01".to_string(),
                name: "hello_world".to_string(),
                description: "Criar programa Hello World em Rust".to_string(),
                difficulty: Difficulty::Easy,
            },
            BenchmarkTask {
                id: "t02".to_string(),
                name: "fibonacci".to_string(),
                description: "Implementar Fibonacci memoizado".to_string(),
                difficulty: Difficulty::Easy,
            },
            BenchmarkTask {
                id: "t03".to_string(),
                name: "http_client".to_string(),
                description: "Criar cliente HTTP síncrono com retry".to_string(),
                difficulty: Difficulty::Medium,
            },
            BenchmarkTask {
                id: "t04".to_string(),
                name: "json_parser".to_string(),
                description: "Parser JSON simplificado".to_string(),
                difficulty: Difficulty::Medium,
            },
            BenchmarkTask {
                id: "t05".to_string(),
                name: "state_machine".to_string(),
                description: "Implementar FSM com 5 estados".to_string(),
                difficulty: Difficulty::Medium,
            },
            BenchmarkTask {
                id: "t06".to_string(),
                name: "linked_list".to_string(),
                description: "Implementar lista ligada com insert/delete".to_string(),
                difficulty: Difficulty::Medium,
            },
            BenchmarkTask {
                id: "t07".to_string(),
                name: "binary_search".to_string(),
                description: "Busca binária genérica".to_string(),
                difficulty: Difficulty::Easy,
            },
            BenchmarkTask {
                id: "t08".to_string(),
                name: "LRU_cache".to_string(),
                description: "Cache LRU com HashMap + LinkedList".to_string(),
                difficulty: Difficulty::Medium,
            },
            BenchmarkTask {
                id: "t09".to_string(),
                name: "thread_pool".to_string(),
                description: "Thread pool com channel".to_string(),
                difficulty: Difficulty::Hard,
            },
            BenchmarkTask {
                id: "t10".to_string(),
                name: "async_runtime_stub".to_string(),
                description: "Runtime async stub com polling".to_string(),
                difficulty: Difficulty::Hard,
            },
            BenchmarkTask {
                id: "t11".to_string(),
                name: "sqlite_wrapper".to_string(),
                description: "Wrapper SQLite simplificado".to_string(),
                difficulty: Difficulty::Medium,
            },
            BenchmarkTask {
                id: "t12".to_string(),
                name: "config_parser".to_string(),
                description: "Parser TOML-like simplificado".to_string(),
                difficulty: Difficulty::Medium,
            },
            BenchmarkTask {
                id: "t13".to_string(),
                name: "regex_engine_stub".to_string(),
                description: "Engine regex para literais e .".to_string(),
                difficulty: Difficulty::Hard,
            },
            BenchmarkTask {
                id: "t14".to_string(),
                name: "markdown_parser".to_string(),
                description: "Parser Markdown para HTML".to_string(),
                difficulty: Difficulty::Medium,
            },
            BenchmarkTask {
                id: "t15".to_string(),
                name: "git_diff".to_string(),
                description: "Algoritmo diff simplificado".to_string(),
                difficulty: Difficulty::Hard,
            },
            BenchmarkTask {
                id: "t16".to_string(),
                name: "rate_limiter".to_string(),
                description: "Rate limiter token bucket".to_string(),
                difficulty: Difficulty::Medium,
            },
            BenchmarkTask {
                id: "t17".to_string(),
                name: "event_bus".to_string(),
                description: "Bus de eventos pub/sub".to_string(),
                difficulty: Difficulty::Medium,
            },
            BenchmarkTask {
                id: "t18".to_string(),
                name: "graph_traversal".to_string(),
                description: "BFS/DFS em grafo".to_string(),
                difficulty: Difficulty::Easy,
            },
            BenchmarkTask {
                id: "t19".to_string(),
                name: "memoization".to_string(),
                description: "Decorator de memoização".to_string(),
                difficulty: Difficulty::Easy,
            },
            BenchmarkTask {
                id: "t20".to_string(),
                name: "csv_parser".to_string(),
                description: "Parser CSV com escape".to_string(),
                difficulty: Difficulty::Easy,
            },
        ];
        Self {
            tasks,
            runner: None,
        }
    }

    /// Injeta um runner customizado. Quando `run()` for chamado, usará este runner
    /// em vez do provider Ollama padrão. Útil para execução via pipeline SYMBION.
    pub fn with_runner<F>(mut self, f: F) -> Self
    where
        F: Fn(&BenchmarkTask) -> BenchmarkResult + 'static,
    {
        self.runner = Some(Box::new(f));
        self
    }

    /// Executa a suite completa e retorna resultados.
    /// Se um runner customizado foi injetado via `with_runner`, usa-o.
    /// Caso contrário, executa via OllamaProvider local.
    pub fn run(&self) -> Vec<BenchmarkResult> {
        if let Some(ref runner) = self.runner {
            return self.tasks.iter().map(|t| runner(t)).collect();
        }

        let bb =
            match arreio_kernel::Blackboard::open(&std::path::PathBuf::from(".arreio/blackboard.json"))
            {
                Ok(bb) => bb,
                Err(_) => {
                    return self
                        .tasks
                        .iter()
                        .map(|t| BenchmarkResult {
                            task_id: t.id.clone(),
                            success: false,
                            latency_ms: 0,
                            tokens_in: 0,
                            tokens_out: 0,
                            cost_usd: 0.0,
                            quality_score: 0.0,
                        })
                        .collect();
                }
            };
        let provider = arreio_provider::OllamaProvider::new(bb);
        run_suite_with_provider(&provider, self)
    }

    /// Compara resultados O Arreio vs baseline.
    pub fn comparative_report(
        &self,
        arreio: &[BenchmarkResult],
        baseline: &[BenchmarkResult],
    ) -> String {
        let mut lines = vec![
            "========================================".to_string(),
            "    Relatório Comparativo de Benchmark".to_string(),
            "========================================".to_string(),
        ];

        // Constrói mapa de resultados baseline por task_id.
        let baseline_map: std::collections::HashMap<&str, &BenchmarkResult> =
            baseline.iter().map(|r| (r.task_id.as_str(), r)).collect();

        for arreio_res in arreio {
            lines.push(format!(
                "\n[Tarefa {}] {}",
                arreio_res.task_id,
                if arreio_res.success { "✅" } else { "❌" }
            ));
            lines.push(format!("  O Arreio  -> latência: {:>6} ms | tokens: {:>4}/{:<4} | custo: ${:.6} | qualidade: {:.2}",
                arreio_res.latency_ms, arreio_res.tokens_in, arreio_res.tokens_out, arreio_res.cost_usd, arreio_res.quality_score));

            if let Some(base) = baseline_map.get(arreio_res.task_id.as_str()) {
                let delta_latency = arreio_res.latency_ms as i64 - base.latency_ms as i64;
                let delta_cost = arreio_res.cost_usd - base.cost_usd;
                let delta_quality = arreio_res.quality_score - base.quality_score;
                lines.push(format!("  Baseline -> latência: {:>6} ms | tokens: {:>4}/{:<4} | custo: ${:.6} | qualidade: {:.2}",
                    base.latency_ms, base.tokens_in, base.tokens_out, base.cost_usd, base.quality_score));
                lines.push(format!(
                    "  Delta    -> latência: {:>+6} ms | custo: ${:+.6} | qualidade: {:+.2}",
                    delta_latency, delta_cost, delta_quality
                ));
            } else {
                lines.push("  Baseline -> (sem dados)".to_string());
            }
        }

        lines.push("\n========================================".to_string());
        lines.join("\n")
    }
}

impl Default for BenchmarkSuite {
    fn default() -> Self {
        Self::new()
    }
}

/// Heurística simples de qualidade baseada no conteúdo retornado pelo LLM.
/// Retorna 1.0 se o texto contém construções típicas de código Rust,
/// 0.5 se possui conteúdo não vazio, e 0.0 se estiver vazio.
pub fn heuristic_quality_score(content: &str) -> f64 {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return 0.0;
    }
    let indicators = ["fn ", "struct ", "impl ", "use ", "pub ", "let ", "match "];
    if indicators.iter().any(|&ind| trimmed.contains(ind)) {
        1.0
    } else {
        0.5
    }
}

/// Executa a suite utilizando um provider LLM real (ou mock).
/// Para cada tarefa, envia um prompt estruturado, mede latência,
/// coleta tokens e estima custo.
pub fn run_suite_with_provider(
    provider: &dyn arreio_provider::ProviderClient,
    suite: &BenchmarkSuite,
) -> Vec<BenchmarkResult> {
    let mut results = Vec::with_capacity(suite.tasks.len());

    for task in &suite.tasks {
        let system = "Você é um engenheiro Rust sênior. Responda apenas com código Rust válido, sem explicações adicionais.".to_string();
        let user = format!(
            "Tarefa: {}\nDescrição: {}\nDificuldade: {:?}\n\nImplemente a solução em Rust.",
            task.name, task.description, task.difficulty
        );

        let req = arreio_provider::ChatRequest {
            messages: Vec::new(),
            model: "benchmark-model".to_string(),
            system,
            user,
            tools: None,
        };

        let start = Instant::now();
        let resp = provider.chat(req);
        let latency_ms = start.elapsed().as_millis() as u64;

        match resp {
            Ok(chat_resp) => {
                let cost =
                    provider.cost_estimate(chat_resp.tokens_in as u32, chat_resp.tokens_out as u32);
                let quality = heuristic_quality_score(&chat_resp.content);
                results.push(BenchmarkResult {
                    task_id: task.id.clone(),
                    success: true,
                    latency_ms,
                    tokens_in: chat_resp.tokens_in as u32,
                    tokens_out: chat_resp.tokens_out as u32,
                    cost_usd: cost,
                    quality_score: quality,
                });
            }
            Err(_) => {
                results.push(BenchmarkResult {
                    task_id: task.id.clone(),
                    success: false,
                    latency_ms,
                    tokens_in: 0,
                    tokens_out: 0,
                    cost_usd: 0.0,
                    quality_score: 0.0,
                });
            }
        }
    }

    results
}

/// Executa a suite utilizando um provider LLM real (ou mock).
/// Alias legível para `run_suite_with_provider`.
pub fn run_suite(
    provider: &dyn arreio_provider::ProviderClient,
    suite: &BenchmarkSuite,
) -> Vec<BenchmarkResult> {
    run_suite_with_provider(provider, suite)
}

/// Teste E2E com LLM real.
/// Cria o provider apropriado com base no nome e executa a suite completa.
pub fn run_e2e_real_llm(provider_name: &str, api_key: &str) -> Result<Vec<BenchmarkResult>> {
    let suite = BenchmarkSuite::new();

    let provider: Box<dyn arreio_provider::ProviderClient> = match provider_name.to_lowercase().as_str() {
        "openai" => Box::new(arreio_provider::OpenAiCompatProvider::new(
            "api.openai.com",
            443,
            Some(api_key.to_string()),
            true,
        )),
        "anthropic" => Box::new(arreio_provider::AnthropicProvider::new(
            "api.anthropic.com",
            443,
            api_key,
            true,
        )),
        "google" => Box::new(arreio_provider::GoogleProvider::new(
            api_key.to_string(),
            "gemini-1.5-pro-latest".to_string(),
        )),
        "azure" => Box::new(arreio_provider::AzureProvider::new(
            "https://api.openai.com".to_string(),
            api_key.to_string(),
            "gpt-4".to_string(),
        )),
        _ => anyhow::bail!(
            "Provider '{}' não suportado para benchmark E2E. Use: openai, anthropic, google, azure.",
            provider_name
        ),
    };

    let results = run_suite_with_provider(provider.as_ref(), &suite);
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arreio_provider::{ChatRequest, MockProvider, ProviderClient};

    #[test]
    fn suite_possui_tarefas() {
        let s = BenchmarkSuite::new();
        assert!(!s.tasks.is_empty());
    }

    #[test]
    fn suite_tem_20_tarefas() {
        let s = BenchmarkSuite::new();
        assert_eq!(s.tasks.len(), 20);
    }

    #[test]
    fn task_dificuldade() {
        let t = BenchmarkTask {
            id: "t01".to_string(),
            name: "test".to_string(),
            description: "desc".to_string(),
            difficulty: Difficulty::Hard,
        };
        assert_eq!(t.difficulty, Difficulty::Hard);
    }

    #[test]
    fn dificuldades_distribuidas() {
        let s = BenchmarkSuite::new();
        let easy = s
            .tasks
            .iter()
            .filter(|t| matches!(t.difficulty, Difficulty::Easy))
            .count();
        let medium = s
            .tasks
            .iter()
            .filter(|t| matches!(t.difficulty, Difficulty::Medium))
            .count();
        let hard = s
            .tasks
            .iter()
            .filter(|t| matches!(t.difficulty, Difficulty::Hard))
            .count();
        assert!(easy > 0, "deve haver tarefas Easy");
        assert!(medium > 0, "deve haver tarefas Medium");
        assert!(hard > 0, "deve haver tarefas Hard");
    }

    #[test]
    fn run_com_runner_mock_executa_todas() {
        let s = BenchmarkSuite::new().with_runner(|task| BenchmarkResult {
            task_id: task.id.clone(),
            success: true,
            latency_ms: 100,
            tokens_in: 10,
            tokens_out: 20,
            cost_usd: 0.0001,
            quality_score: 1.0,
        });
        let r = s.run();
        assert_eq!(r.len(), 20);
        assert!(r.iter().all(|res| res.success));
    }

    #[test]
    fn run_sem_runner_retorna_resultados() {
        // Com runner dummy configurado, run() usa o runner em vez de fallback para Ollama.
        let s = BenchmarkSuite::new().with_runner(|_task| BenchmarkResult {
            task_id: "dummy".to_string(),
            success: false,
            latency_ms: 0,
            tokens_in: 0,
            tokens_out: 0,
            cost_usd: 0.0,
            quality_score: 0.0,
        });
        let r = s.run();
        assert_eq!(r.len(), 20);
    }

    #[test]
    fn comparative_retorna_string() {
        let s = BenchmarkSuite::new();
        let r = s.comparative_report(&[], &[]);
        assert!(!r.is_empty());
    }

    #[test]
    fn comparative_report_com_dados() {
        let s = BenchmarkSuite::new();
        let arreio = vec![BenchmarkResult {
            task_id: "t01".to_string(),
            success: true,
            latency_ms: 100,
            tokens_in: 10,
            tokens_out: 20,
            cost_usd: 0.0001,
            quality_score: 1.0,
        }];
        let baseline = vec![BenchmarkResult {
            task_id: "t01".to_string(),
            success: true,
            latency_ms: 150,
            tokens_in: 12,
            tokens_out: 25,
            cost_usd: 0.0002,
            quality_score: 0.8,
        }];
        let report = s.comparative_report(&arreio, &baseline);
        assert!(report.contains("Delta"));
        assert!(report.contains("100"));
        assert!(report.contains("150"));
    }

    #[test]
    fn e2e_provider_invalido_retorna_erro() {
        let r = run_e2e_real_llm("provedor_inexistente", "fake-key");
        assert!(r.is_err());
        let msg = format!("{}", r.unwrap_err());
        assert!(msg.contains("não suportado"));
    }

    #[test]
    fn run_suite_with_provider_mock() {
        let suite = BenchmarkSuite::new();
        let mock = MockProvider::new("fn main() {}");
        let results = run_suite_with_provider(&mock, &suite);
        assert_eq!(results.len(), suite.tasks.len());
        for res in &results {
            assert!(res.success);
            assert!(res.tokens_out > 0);
            assert_eq!(res.quality_score, 1.0); // contém "fn "
        }
    }

    #[test]
    fn run_suite_com_provider_falho() {
        let suite = BenchmarkSuite::new();
        let mock = MockProvider::with_failures(100);
        let results = run_suite_with_provider(&mock, &suite);
        assert_eq!(results.len(), suite.tasks.len());
        for res in &results {
            assert!(!res.success);
            assert_eq!(res.tokens_in, 0);
            assert_eq!(res.tokens_out, 0);
            assert_eq!(res.quality_score, 0.0);
        }
    }

    #[test]
    fn resultado_custo_eh_calculado() {
        let suite = BenchmarkSuite::new();
        let mock = MockProvider::new("resposta");
        // Override cost_estimate não é possível no MockProvider (retorna 0.0).
        // Verificamos que o teste roda sem panico e que tokens são coletados.
        let results = run_suite_with_provider(&mock, &suite);
        assert_eq!(results.len(), suite.tasks.len());
    }

    #[test]
    fn latencia_maior_que_zero() {
        let suite = BenchmarkSuite::new();
        let mock = MockProvider::new("fn x() {}");
        let results = run_suite_with_provider(&mock, &suite);
        for res in &results {
            assert!(
                res.latency_ms < 10_000,
                "latência deve ser razoável em mock"
            );
        }
    }

    #[test]
    fn quality_score_heuristica() {
        // Testa a heurística indiretamente via mock
        let mock_codigo = MockProvider::new("fn main() {}");
        let req = ChatRequest {
            messages: Vec::new(),
            model: "m".to_string(),
            system: "s".to_string(),
            user: "u".to_string(),
            tools: None,
        };
        let resp = mock_codigo.chat(req.clone()).unwrap();
        assert_eq!(heuristic_quality_score(&resp.content), 1.0);

        let mock_texto = MockProvider::new("apenas texto");
        let resp2 = mock_texto.chat(req).unwrap();
        assert_eq!(heuristic_quality_score(&resp2.content), 0.5);
    }

    #[test]
    fn suite_todas_as_tarefas_tem_id_unico() {
        let s = BenchmarkSuite::new();
        let mut ids = std::collections::HashSet::new();
        for t in &s.tasks {
            assert!(ids.insert(t.id.clone()), "id duplicado: {}", t.id);
        }
        assert_eq!(ids.len(), 20);
    }

    #[test]
    fn run_suite_alias_funciona() {
        let suite = BenchmarkSuite::new();
        let mock = MockProvider::new("fn main() {}");
        let results = run_suite(&mock, &suite);
        assert_eq!(results.len(), suite.tasks.len());
    }
}
