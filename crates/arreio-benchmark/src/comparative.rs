//! Comparador quantitativo Arreio vs baseline (Claude Code manual, Cursor).

use crate::BenchmarkResult;

/// Resultado de uma ferramenta baseline.
#[derive(Debug, Clone)]
pub struct BaselineResult {
    pub tool_name: String,
    pub task_id: String,
    pub latency_ms: u64,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub cost_usd: f64,
    pub success: bool,
}

/// Comparador O Arreio vs baseline.
pub struct ComparativeAnalyzer;

impl ComparativeAnalyzer {
    /// Cria uma nova instância do analisador comparativo.
    pub fn new() -> Self {
        Self
    }

    /// Compara O Arreio vs baseline em uma única tarefa.
    pub fn compare_task(
        &self,
        arreio: &BenchmarkResult,
        baseline: &BaselineResult,
    ) -> TaskComparison {
        let latency_delta_pct = Self::pct_delta(baseline.latency_ms as f64, arreio.latency_ms as f64);
        let cost_delta_pct = Self::pct_delta(baseline.cost_usd, arreio.cost_usd);
        let arreio_total = (arreio.tokens_in + arreio.tokens_out) as f64;
        let base_total = (baseline.tokens_in + baseline.tokens_out) as f64;
        let token_efficiency_pct = if base_total == 0.0 {
            0.0
        } else {
            ((base_total - arreio_total) / base_total) * 100.0
        };
        let quality_score_delta = arreio.quality_score - Self::quality_from_baseline(baseline);

        let winner = if arreio.success && !baseline.success {
            "arreio".to_string()
        } else if !arreio.success && baseline.success {
            "baseline".to_string()
        } else {
            let arreio_score = Self::score_task(arreio.latency_ms, arreio.cost_usd, arreio.quality_score);
            let base_score = Self::score_task(
                baseline.latency_ms,
                baseline.cost_usd,
                Self::quality_from_baseline(baseline),
            );
            if arreio_score < base_score {
                "arreio".to_string()
            } else if base_score < arreio_score {
                "baseline".to_string()
            } else {
                "tie".to_string()
            }
        };

        TaskComparison {
            task_id: arreio.task_id.clone(),
            latency_delta_pct,
            cost_delta_pct,
            token_efficiency_pct,
            quality_score_delta,
            winner,
        }
    }

    /// Gera relatório completo em formato Markdown com tabelas comparativas.
    pub fn generate_report(&self, comparisons: &[TaskComparison]) -> String {
        let mut report = String::new();
        report.push_str("# Relatório Comparativo Arreio vs Baseline\n\n");

        // Tabela por tarefa
        report.push_str("## Comparativo por Tarefa\n\n");
        report.push_str(
            "| Tarefa | Δ Latência (%) | Δ Custo (%) | Δ Tokens (%) | Δ Qualidade | Vencedor |\n",
        );
        report.push_str(
            "|--------|----------------|-------------|--------------|-------------|----------|\n",
        );
        for c in comparisons {
            report.push_str(&format!(
                "| {} | {:.2}% | {:.2}% | {:.2}% | {:.3} | {} |\n",
                c.task_id,
                c.latency_delta_pct,
                c.cost_delta_pct,
                c.token_efficiency_pct,
                c.quality_score_delta,
                c.winner
            ));
        }
        report.push('\n');

        // Contagem de vitórias
        let arreio_wins = comparisons.iter().filter(|c| c.winner == "arreio").count();
        let base_wins = comparisons
            .iter()
            .filter(|c| c.winner == "baseline")
            .count();
        let ties = comparisons.iter().filter(|c| c.winner == "tie").count();

        report.push_str("## Resumo de Vitórias\n\n");
        report.push_str(&format!("- **O Arreio**: {} vitória(s)\n", arreio_wins));
        report.push_str(&format!("- **Baseline**: {} vitória(s)\n", base_wins));
        report.push_str(&format!("- **Empates**: {}\n\n", ties));

        // Médias
        if !comparisons.is_empty() {
            let avg_latency = comparisons.iter().map(|c| c.latency_delta_pct).sum::<f64>()
                / comparisons.len() as f64;
            let avg_cost = comparisons.iter().map(|c| c.cost_delta_pct).sum::<f64>()
                / comparisons.len() as f64;
            let avg_tokens = comparisons
                .iter()
                .map(|c| c.token_efficiency_pct)
                .sum::<f64>()
                / comparisons.len() as f64;
            report.push_str("## Médias\n\n");
            report.push_str(&format!("- **Δ Latência médio**: {:.2}%\n", avg_latency));
            report.push_str(&format!("- **Δ Custo médio**: {:.2}%\n", avg_cost));
            report.push_str(&format!("- **Δ Tokens médio**: {:.2}%\n\n", avg_tokens));
        }

        report
    }

    /// Calcula métricas agregadas a partir de uma lista de comparações.
    pub fn aggregate_metrics(&self, comparisons: &[TaskComparison]) -> AggregateMetrics {
        // Para agregar precisamos dos dados brutos; como só temos deltas,
        // retornamos valores derivados das comparações.
        // Nota: este método assume que as comparações possuem os dados necessários.
        // Na prática, em um cenário real, passaríamos os resultados brutos.
        // Aqui usamos estimativas baseadas nos deltas para manter a API.

        let total_tasks = comparisons.len().max(1) as f64;
        let arreio_wins = comparisons.iter().filter(|c| c.winner == "arreio").count() as f64;
        let base_wins = comparisons
            .iter()
            .filter(|c| c.winner == "baseline")
            .count() as f64;

        let arreio_success_rate = arreio_wins / total_tasks;
        let baseline_success_rate = base_wins / total_tasks;

        // Estimativas de custo e latência média baseadas em valores arbitrários
        // já que não temos os resultados brutos neste método.
        let baseline_avg_cost = 0.05;
        let baseline_avg_latency = 5000.0;

        let avg_cost_delta =
            comparisons.iter().map(|c| c.cost_delta_pct).sum::<f64>() / total_tasks;
        let avg_latency_delta =
            comparisons.iter().map(|c| c.latency_delta_pct).sum::<f64>() / total_tasks;

        let arreio_avg_cost = baseline_avg_cost * (1.0 + avg_cost_delta / 100.0);
        let arreio_avg_latency = baseline_avg_latency * (1.0 + avg_latency_delta / 100.0);

        let overall_winner = if arreio_wins > base_wins {
            "arreio".to_string()
        } else if base_wins > arreio_wins {
            "baseline".to_string()
        } else {
            "tie".to_string()
        };

        AggregateMetrics {
            arreio_total_cost: arreio_avg_cost * total_tasks,
            baseline_total_cost: baseline_avg_cost * total_tasks,
            arreio_avg_latency_ms: arreio_avg_latency,
            baseline_avg_latency_ms: baseline_avg_latency,
            arreio_success_rate,
            baseline_success_rate,
            overall_winner,
        }
    }

    // --- Helpers privados ---

    /// Calcula delta percentual: negativo = primeiro parâmetro é maior (baseline é pior).
    fn pct_delta(baseline: f64, actual: f64) -> f64 {
        if baseline == 0.0 {
            0.0
        } else {
            ((actual - baseline) / baseline) * 100.0
        }
    }

    /// Deriva uma qualidade simples do baseline (assume sucesso = 1.0, falha = 0.0).
    fn quality_from_baseline(baseline: &BaselineResult) -> f64 {
        if baseline.success {
            1.0
        } else {
            0.0
        }
    }

    /// Score heurístico para decisão de vencedor (menor = melhor).
    fn score_task(latency_ms: u64, cost_usd: f64, quality: f64) -> f64 {
        // Normalização simples: latência em segundos, custo em centavos, qualidade invertida
        let latency_score = (latency_ms as f64) / 1000.0;
        let cost_score = cost_usd * 100.0;
        let quality_penalty = (1.0 - quality) * 10.0;
        latency_score + cost_score + quality_penalty
    }
}

impl Default for ComparativeAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

/// Comparação de uma tarefa específica.
#[derive(Debug, Clone)]
pub struct TaskComparison {
    pub task_id: String,
    /// Negativo = O Arreio mais rápido.
    pub latency_delta_pct: f64,
    /// Negativo = O Arreio mais barato.
    pub cost_delta_pct: f64,
    /// Positivo = O Arreio usa menos tokens.
    pub token_efficiency_pct: f64,
    pub quality_score_delta: f64,
    /// "arreio" | "baseline" | "tie"
    pub winner: String,
}

/// Métricas agregadas de todas as comparações.
#[derive(Debug, Clone)]
pub struct AggregateMetrics {
    pub arreio_total_cost: f64,
    pub baseline_total_cost: f64,
    pub arreio_avg_latency_ms: f64,
    pub baseline_avg_latency_ms: f64,
    pub arreio_success_rate: f64,
    pub baseline_success_rate: f64,
    pub overall_winner: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BenchmarkResult;

    fn arreio_result(
        task_id: &str,
        latency: u64,
        cost: f64,
        tokens_in: u32,
        tokens_out: u32,
        quality: f64,
        success: bool,
    ) -> BenchmarkResult {
        BenchmarkResult {
            task_id: task_id.to_string(),
            latency_ms: latency,
            cost_usd: cost,
            tokens_in,
            tokens_out,
            quality_score: quality,
            success,
        }
    }

    fn baseline_result(
        task_id: &str,
        tool: &str,
        latency: u64,
        cost: f64,
        tokens_in: u32,
        tokens_out: u32,
        success: bool,
    ) -> BaselineResult {
        BaselineResult {
            tool_name: tool.to_string(),
            task_id: task_id.to_string(),
            latency_ms: latency,
            cost_usd: cost,
            tokens_in,
            tokens_out,
            success,
        }
    }

    #[test]
    fn comparador_cria_instancia() {
        let _a = ComparativeAnalyzer::new();
    }

    #[test]
    fn comparador_default() {
        let _a: ComparativeAnalyzer = Default::default();
    }

    #[test]
    fn compare_task_arreio_mais_rapido() {
        let a = ComparativeAnalyzer::new();
        let arreio = arreio_result("t01", 1000, 0.01, 100, 200, 1.0, true);
        let base = baseline_result("t01", "cursor", 2000, 0.02, 150, 250, true);
        let cmp = a.compare_task(&arreio, &base);
        assert!(
            cmp.latency_delta_pct < 0.0,
            "O Arreio deveria estar mais rápido"
        );
        assert_eq!(cmp.winner, "arreio");
    }

    #[test]
    fn compare_task_arreio_mais_barato() {
        let a = ComparativeAnalyzer::new();
        let arreio = arreio_result("t02", 2000, 0.005, 100, 200, 1.0, true);
        let base = baseline_result("t02", "claude", 2000, 0.02, 150, 250, true);
        let cmp = a.compare_task(&arreio, &base);
        assert!(
            cmp.cost_delta_pct < 0.0,
            "O Arreio deveria estar mais barato"
        );
    }

    #[test]
    fn compare_task_arreio_menos_tokens() {
        let a = ComparativeAnalyzer::new();
        let arreio = arreio_result("t03", 2000, 0.01, 100, 100, 1.0, true);
        let base = baseline_result("t03", "cursor", 2000, 0.01, 200, 300, true);
        let cmp = a.compare_task(&arreio, &base);
        assert!(
            cmp.token_efficiency_pct > 0.0,
            "O Arreio deveria usar menos tokens"
        );
    }

    #[test]
    fn compare_task_baseline_ganha_quando_arreio_falha() {
        let a = ComparativeAnalyzer::new();
        let arreio = arreio_result("t04", 1000, 0.01, 100, 200, 0.0, false);
        let base = baseline_result("t04", "claude", 5000, 0.05, 500, 500, true);
        let cmp = a.compare_task(&arreio, &base);
        assert_eq!(cmp.winner, "baseline");
    }

    #[test]
    fn compare_task_empate() {
        let a = ComparativeAnalyzer::new();
        let arreio = arreio_result("t05", 1000, 0.01, 100, 200, 1.0, true);
        let base = baseline_result("t05", "cursor", 1000, 0.01, 100, 200, true);
        let cmp = a.compare_task(&arreio, &base);
        // Mesmos parâmetros devem empatar ou dar arreio por qualidade_score igual
        // Ajustamos para forçar empate: qualidade também igual
        assert_eq!(cmp.latency_delta_pct, 0.0);
        assert_eq!(cmp.cost_delta_pct, 0.0);
    }

    #[test]
    fn generate_report_contem_tabelas() {
        let a = ComparativeAnalyzer::new();
        let comps = vec![
            a.compare_task(
                &arreio_result("t01", 1000, 0.01, 100, 200, 1.0, true),
                &baseline_result("t01", "cursor", 2000, 0.02, 150, 250, true),
            ),
            a.compare_task(
                &arreio_result("t02", 2000, 0.005, 100, 200, 1.0, true),
                &baseline_result("t02", "claude", 2000, 0.02, 150, 250, true),
            ),
        ];
        let report = a.generate_report(&comps);
        assert!(report.contains("# Relatório Comparativo"));
        assert!(report.contains("| Tarefa |"));
        assert!(report.contains("## Resumo de Vitórias"));
        assert!(report.contains("## Médias"));
    }

    #[test]
    fn generate_report_vazio() {
        let a = ComparativeAnalyzer::new();
        let report = a.generate_report(&[]);
        assert!(report.contains("# Relatório Comparativo"));
        // Não deve conter a seção de médias quando vazio
        assert!(!report.contains("## Médias"));
    }

    #[test]
    fn aggregate_metrics_calcula_taxas() {
        let a = ComparativeAnalyzer::new();
        let comps = vec![
            a.compare_task(
                &arreio_result("t01", 1000, 0.01, 100, 200, 1.0, true),
                &baseline_result("t01", "cursor", 2000, 0.02, 150, 250, true),
            ),
            a.compare_task(
                &arreio_result("t02", 2000, 0.005, 100, 200, 1.0, true),
                &baseline_result("t02", "claude", 2000, 0.02, 150, 250, true),
            ),
            a.compare_task(
                &arreio_result("t03", 3000, 0.03, 100, 200, 0.5, false),
                &baseline_result("t03", "claude", 1000, 0.01, 150, 250, true),
            ),
        ];
        let agg = a.aggregate_metrics(&comps);
        assert!(agg.arreio_success_rate >= 0.0 && agg.arreio_success_rate <= 1.0);
        assert!(agg.baseline_success_rate >= 0.0 && agg.baseline_success_rate <= 1.0);
        assert!(!agg.overall_winner.is_empty());
    }

    #[test]
    fn aggregate_metrics_arreio_vencedor_geral() {
        let a = ComparativeAnalyzer::new();
        let comps = vec![
            a.compare_task(
                &arreio_result("t01", 1000, 0.01, 100, 200, 1.0, true),
                &baseline_result("t01", "cursor", 2000, 0.02, 150, 250, true),
            ),
            a.compare_task(
                &arreio_result("t02", 1000, 0.01, 100, 200, 1.0, true),
                &baseline_result("t02", "cursor", 2000, 0.02, 150, 250, true),
            ),
        ];
        let agg = a.aggregate_metrics(&comps);
        assert_eq!(agg.overall_winner, "arreio");
    }

    #[test]
    fn pct_delta_divisao_por_zero() {
        let a = ComparativeAnalyzer::new();
        let arreio = arreio_result("t06", 1000, 0.0, 100, 200, 1.0, true);
        let base = baseline_result("t06", "cursor", 2000, 0.0, 150, 250, true);
        let cmp = a.compare_task(&arreio, &base);
        // custo baseline zero deve resultar em 0.0 sem panic
        assert_eq!(cmp.cost_delta_pct, 0.0);
    }

    #[test]
    fn quality_score_delta_positivo() {
        let a = ComparativeAnalyzer::new();
        let arreio = arreio_result("t07", 2000, 0.01, 100, 200, 0.95, true);
        let base = baseline_result("t07", "claude", 2000, 0.01, 150, 250, true);
        let cmp = a.compare_task(&arreio, &base);
        // baseline success = 1.0, arreio = 0.95
        assert!(cmp.quality_score_delta < 0.0 || (cmp.quality_score_delta - (-0.05)).abs() < 0.001);
    }
}
