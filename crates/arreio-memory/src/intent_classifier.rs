//! IntentClassifier — classificação automática de intenção do usuário.
//!
//! Traduz a mensagem do usuário em uma intenção (Conversacional, Task, Hybrid)
//! usando heurísticas léxicas determinísticas (regex + palavras-chave).
//!
//! Princípio: o sistema decide, o usuário conversa.
//! Sem LLM — 100% local, determinístico, instantâneo.

/// Intenção detectada na mensagem do usuário.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserIntent {
    /// Pergunta, explicação, discussão, saudação.
    Conversational,
    /// Pedido de ação: criar arquivo, modificar código, executar, gerar relatório.
    Task,
    /// Conversação que contém uma tarefa embutida.
    Hybrid,
}

/// Resultado da classificação com confiança.
#[derive(Debug, Clone, PartialEq)]
pub struct IntentResult {
    pub intent: UserIntent,
    /// Confiança entre 0.0 e 1.0.
    pub confidence: f32,
    /// Razão da classificação (para debug).
    pub reason: String,
}

/// Classificador de intenção baseado em heurísticas léxicas.
pub struct IntentClassifier;

impl IntentClassifier {
    /// Cria um novo classificador.
    pub fn new() -> Self {
        Self
    }

    /// Classifica uma mensagem do usuário.
    pub fn classify(&self, input: &str) -> IntentResult {
        let normalized = input.to_lowercase();
        let words: Vec<&str> = normalized
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|w| !w.is_empty())
            .collect();

        let word_count = words.len();

        // ── Scores por categoria ─────────────────────────────────────────────
        let mut task_score: f32 = 0.0;
        let mut conv_score: f32 = 0.0;
        let mut reasons = Vec::new();

        // 1. Palavras-chave de task (palavras simples — acessíveis após split)
        let task_keywords = [
            "crie",
            "criar",
            "gerar",
            "generate",
            "execute",
            "executar",
            "modifique",
            "modificar",
            "alterar",
            "altere",
            "faça",
            "fazer",
            "implemente",
            "implementar",
            "processe",
            "processar",
            "analisar",
            "analise",
            "relatório",
            "relatorio",
            "planilha",
            "arquivo",
            "file",
            "script",
            "código",
            "codigo",
            "code",
            "função",
            "funcao",
            "function",
            "classe",
            "class",
            "módulo",
            "modulo",
            "module",
            "teste",
            "test",
            "deploy",
            "build",
            "compile",
            "compilar",
            "instalar",
            "install",
            "configurar",
            "configure",
            "setup",
            "inicializar",
            "init",
            "atualizar",
            "update",
            "deletar",
            "delete",
            "remover",
            "remove",
            "converter",
            "convert",
            "exportar",
            "export",
            "importar",
            "import",
            "backup",
            "restaurar",
            "restore",
            "sincronizar",
            "sync",
            "validar",
            "validate",
            "verificar",
            "check",
            "corrigir",
            "fix",
            "otimizar",
            "optimize",
            "refatorar",
            "refactor",
            "documentar",
            "document",
            "escrever",
            "write",
            "salvar",
            "save",
            "enviar",
            "send",
            "receber",
            "receive",
            "listar",
            "list",
            "mostrar",
            "show",
            "exibir",
            "display",
            "imprimir",
            "print",
            "calcular",
            "calculate",
            "somar",
            "sum",
            "média",
            "media",
            "average",
            "total",
            "contar",
            "count",
            "filtrar",
            "filter",
            "ordenar",
            "sort",
            "agrupar",
            "group",
            "buscar",
            "search",
            "find",
            "encontrar",
            "localizar",
            "locate",
            "procurar",
            "look",
            "consultar",
            "query",
            "select",
            "insert",
            "create",
            "drop",
            "alter",
            "grant",
            "revoke",
            "commit",
            "merge",
            "join",
            "union",
            "view",
            "trigger",
            "procedure",
            "index",
            "constraint",
            "primary",
            "foreign",
            "unique",
            "default",
            "sequence",
            "cursor",
            "transaction",
            "savepoint",
            "release",
            "lock",
            "unlock",
            "deadlock",
            "isolation",
            "serializable",
            "repeatable",
            "read",
            "committed",
            "uncommitted",
            "dirty",
            "phantom",
            "lost",
            "normalizar",
            "denormalize",
            "star",
            "schema",
            "snowflake",
            "dimensional",
            "fact",
            "dimension",
            "cube",
            "rollup",
            "drill",
            "slice",
            "dice",
            "pivot",
            "unpivot",
            "cte",
            "recursive",
            "window",
            "over",
            "partition",
            "rank",
            "row_number",
            "dense_rank",
            "lag",
            "lead",
            "first_value",
            "last_value",
            "nth_value",
            "percentile",
            "quartile",
            "decile",
            "histogram",
            "bin",
            "bucket",
            "frequency",
            "cumulative",
            "running",
            "moving",
            "exponential",
            "smoothing",
            "forecast",
            "predict",
            "trend",
            "seasonal",
            "cyclical",
            "irregular",
            "residual",
            "mae",
            "mse",
            "rmse",
            "mape",
            "smape",
            "r2",
            "adjusted",
            "aic",
            "bic",
            "deviance",
            "entropy",
            "information",
            "gain",
            "gini",
            "impurity",
            "split",
            "prune",
            "boost",
            "bag",
            "stack",
            "blend",
            "ensemble",
            "voting",
            "averaging",
            "weighted",
            "meta",
            "learner",
            "base",
            "weak",
            "strong",
            "classifier",
            "regressor",
            "cluster",
            "reduction",
            "pca",
            "svd",
            "lda",
            "tsne",
            "umap",
            "autoencoder",
            "gan",
            "vae",
            "diffusion",
            "transformer",
            "attention",
            "bert",
            "gpt",
            "llama",
            "gemma",
            "mistral",
            "assistant",
            "chatbot",
            "agent",
            "bot",
            "ia",
            "ai",
            "inteligência",
            "inteligencia",
            "artificial",
            "machine",
            "learning",
            "deep",
            "reinforcement",
            "supervised",
            "unsupervised",
            "semi",
            "self",
            "few",
            "zero",
            "shot",
            "prompt",
            "engineering",
            "tuning",
            "fine",
            "transfer",
            "domain",
            "adaptation",
            "distillation",
            "pruning",
            "quantization",
            "compression",
            "optimization",
            "inference",
            "training",
            "epoch",
            "batch",
            "iteration",
            "step",
            "gradient",
            "descent",
            "backpropagation",
            "loss",
            "objective",
            "regularization",
            "dropout",
            "normalization",
            "activation",
            "relu",
            "sigmoid",
            "tanh",
            "softmax",
            "cross_entropy",
            "binary",
            "multiclass",
            "multilabel",
            "hierarchical",
            "imbalanced",
            "oversampling",
            "undersampling",
            "smote",
            "adasyn",
            "cost_sensitive",
            "threshold",
            "calibration",
            "platt",
            "isotonic",
            "scaling",
            "standardization",
            "min_max",
            "robust",
            "log",
            "power",
            "box_cox",
            "yeo_johnson",
            "quantile",
            "binning",
            "encoding",
            "dummy",
            "label",
            "ordinal",
            "target",
            "frequency",
            "hashing",
            "embedding",
            "word2vec",
            "glove",
            "fasttext",
            "elmo",
            "sentence",
            "transformers",
            "tokenizer",
            "vocab",
            "corpus",
            "document",
            "term",
            "tfidf",
            "bm25",
            "lsa",
            "nmf",
            "hdp",
            "word",
            "topic",
            "theme",
            "concept",
            "entity",
            "relation",
            "triple",
            "knowledge",
            "graph",
            "ontology",
            "rdf",
            "sparql",
            "owl",
            "reasoning",
            "translational",
            "rotat",
            "complex",
            "distmult",
            "conve",
            "compgcn",
            "rgcn",
            "gat",
            "gcn",
            "graphsage",
            "pin",
            "node2vec",
            "deepwalk",
            "line",
            "sdne",
            "struc2vec",
            "metapath2vec",
            "han",
            "hetgnn",
            "hgt",
            "graphormer",
            "graphtrans",
        ];

        for (i, word) in words.iter().enumerate() {
            if task_keywords.contains(word) {
                task_score += 0.25;
                reasons.push(format!("task_keyword:{}", word));
            }
            // Extra pontuação se a palavra está no início (primeiras 3 posições)
            if i < 3 && task_keywords.contains(word) {
                task_score += 0.15;
            }
        }

        // 2. Palavras-chave conversacionais
        // Divididas em: (a) palavras simples (match por token) e (b) frases (match por substring)
        let conv_single = [
            "oque",
            "qual",
            "quais",
            "quem",
            "quando",
            "onde",
            "como",
            "porque",
            "porquê",
            "porquê",
            "explique",
            "explica",
            "conta",
            "diga",
            "fale",
            "oi",
            "olá",
            "ola",
            "ei",
            "beleza",
            "blz",
            "valeu",
            "obrigado",
            "obrigada",
            "thanks",
            "thank",
            "agradeço",
            "grato",
            "grata",
            "desculpe",
            "desculpa",
            "perdão",
            "sinto",
            "sorry",
            "opa",
            "eai",
            "fala",
            "salve",
            "hello",
            "hi",
            "hey",
            "entendo",
            "entendi",
            "compreendo",
            "compreendi",
            "saquei",
            "captei",
            "peguei",
            "got",
            "understand",
            "understood",
            "clear",
            "claro",
            "certo",
            "ok",
            "okay",
            "bele",
            "show",
            "perfeito",
            "excelente",
            "ótimo",
            "otimo",
            "bom",
            "legal",
            "nice",
            "great",
            "awesome",
            "incredible",
            "incrível",
            "incrivel",
            "maravilha",
            "interessante",
            "curioso",
            "estranho",
            "estranha",
            "diferente",
            "similar",
            "parecido",
            "igual",
            "mesmo",
            "outro",
            "outra",
            "talvez",
            "possivelmente",
            "provavelmente",
            "provavel",
            "maybe",
            "perhaps",
            "probably",
            "definitely",
            "certamente",
            "absolutamente",
            "totalmente",
            "parcialmente",
            "muito",
            "pouco",
            "mais",
            "menos",
            "tanto",
            "quanto",
            "quase",
            "aproximadamente",
            "cerca",
            "algo",
            "alguma",
            "algum",
            "nada",
            "ninguém",
            "ninguem",
            "tudo",
            "todos",
            "todas",
            "cada",
            "qualquer",
            "nenhum",
            "nenhuma",
            "sempre",
            "nunca",
            "já",
            "ja",
            "ainda",
            "também",
            "tambem",
            "só",
            "so",
            "apenas",
            "somente",
            "exclusivamente",
            "inclusive",
            "exceto",
            "salvo",
            "depende",
            "tal",
            "assim",
            "desse",
            "deste",
            "daquele",
            "disto",
            "disso",
            "daquilo",
            "aqui",
            "aí",
            "ai",
            "ali",
            "lá",
            "la",
            "aonde",
            "donde",
            "enquanto",
            "desde",
            "até",
            "ate",
            "antes",
            "depois",
            "durante",
            "após",
            "apos",
            "logo",
            "imediatamente",
            "eventualmente",
            "finalmente",
            "caso",
            "supondo",
            "suponha",
            "imagine",
            "pense",
            "considere",
            "considerando",
            "pois",
            "porquanto",
            "embora",
            "conquanto",
            "sequer",
            "mesmo",
            "ainda",
        ];

        let conv_phrases = [
            "o que",
            "por que",
            "me diga",
            "me fale",
            "me conta",
            "bom dia",
            "boa tarde",
            "boa noite",
            "tudo bem",
            "como vai",
            "e aí",
            "que bom",
            "que legal",
            "que pena",
            "que chato",
            "que estranho",
            "sem dúvida",
            "sem duvida",
            "com certeza",
            "um pouco",
            "bastante",
            "por fim",
            "em suma",
            "resumindo",
            "resumo",
            "concluindo",
            "conclusão",
            "conclusao",
            "resumidamente",
            "brevemente",
            "em geral",
            "geralmente",
            "normalmente",
            "tipicamente",
            "geralmente falando",
            "na maioria",
            "na maior parte",
            "em princípio",
            "em principio",
            "teoricamente",
            "praticamente",
            "na prática",
            "na pratica",
            "na verdade",
            "de fato",
            "realmente",
            "verdadeiramente",
            "efetivamente",
            "concretamente",
            "especificamente",
            "particularmente",
            "especialmente",
            "principalmente",
            "sobretudo",
            "acima de tudo",
            "antes de mais nada",
            "em primeiro lugar",
            "em segundo lugar",
            "por um lado",
            "por outro lado",
            "por último",
            "para concluir",
            "em conclusão",
            "em conclusao",
            "em resumo",
            "para resumir",
            "em síntese",
            "em sintese",
            "sintetizando",
            "de modo geral",
            "de forma geral",
            "de maneira geral",
            "por exemplo",
            "por ex",
            "ex",
            "exemplificando",
            "por instance",
            "por caso",
            "no caso de",
            "em caso de",
            "dado que",
            "visto que",
            "uma vez que",
            "posto que",
            "já que",
            "ja que",
            "uma vez",
            "desde que",
            "contanto que",
            "a menos que",
            "a nao ser que",
            "exceto se",
            "salvo se",
            "sem que",
            "para que",
            "a fim de que",
            "de modo que",
            "de forma que",
            "de maneira que",
            "tal que",
            "tão que",
            "tanto que",
            "assim que",
            "logo que",
            "tão logo",
            "tão logo que",
            "mal que",
            "apenas que",
            "somente que",
            "nem que",
            "ainda que",
            "por mais que",
            "por menos que",
            "por muito que",
            "por pouco que",
            "seja qual for",
            "seja quem for",
            "seja o que for",
            "qualquer que seja",
            "quemquer que seja",
            "o que for",
            "onde for",
            "como for",
            "quando for",
            "quanto for",
        ];

        // (a) Palavras simples: match por token
        for word in &words {
            if conv_single.contains(word) {
                conv_score += 0.20;
                reasons.push(format!("conv_single:{}", word));
            }
        }

        // (b) Frases: match por substring no texto normalizado
        for phrase in &conv_phrases {
            if normalized.contains(phrase) {
                conv_score += 0.20;
                reasons.push(format!("conv_phrase:{}", phrase));
            }
        }

        // 3. Detecção de saudações (forte sinal conversacional)
        let greetings = [
            "bom dia",
            "boa tarde",
            "boa noite",
            "oi",
            "olá",
            "ola",
            "ei",
            "hello",
            "hi",
            "hey",
            "salve",
            "fala",
            "e aí",
            "eai",
        ];
        for greeting in &greetings {
            if normalized.starts_with(*greeting) {
                conv_score += 0.40;
                reasons.push(format!("greeting:{}", greeting));
            }
        }

        // 4. Detecção de agradecimentos/despedidas (forte sinal conversacional)
        let closings = [
            "obrigado",
            "obrigada",
            "valeu",
            "thanks",
            "thank you",
            "grato",
            "grata",
            "tchau",
            "adeus",
            "até logo",
            "ate logo",
            "flw",
            "falou",
        ];
        for closing in &closings {
            if normalized.contains(closing) {
                conv_score += 0.30;
                reasons.push(format!("closing:{}", closing));
            }
        }

        // 5. Presença de paths de arquivo (forte sinal de task)
        if normalized.contains('.') {
            let file_exts = [
                ".rs",
                ".py",
                ".js",
                ".ts",
                ".java",
                ".c",
                ".cpp",
                ".h",
                ".go",
                ".rb",
                ".php",
                ".swift",
                ".kt",
                ".scala",
                ".r",
                ".m",
                ".sql",
                ".html",
                ".css",
                ".scss",
                ".sass",
                ".less",
                ".xml",
                ".json",
                ".yaml",
                ".yml",
                ".toml",
                ".ini",
                ".cfg",
                ".conf",
                ".md",
                ".txt",
                ".csv",
                ".tsv",
                ".xlsx",
                ".xls",
                ".ods",
                ".docx",
                ".doc",
                ".odt",
                ".pdf",
                ".pptx",
                ".ppt",
                ".odp",
                ".zip",
                ".tar",
                ".gz",
                ".bz2",
                ".7z",
                ".rar",
                ".dockerfile",
                ".sh",
                ".bash",
                ".zsh",
                ".fish",
                ".ps1",
                ".bat",
                ".cmd",
                ".makefile",
                ".mk",
                ".cmake",
                ".gradle",
                ".sbt",
                ".pom",
            ];
            for ext in &file_exts {
                if normalized.contains(ext) {
                    task_score += 0.50;
                    reasons.push(format!("file_ext:{}", ext));
                }
            }
        }

        // 6. Verbos imperativos no início (forte sinal de task)
        let imperative_verbs = [
            "crie",
            "criar",
            "faça",
            "fazer",
            "execute",
            "executar",
            "modifique",
            "modificar",
            "altere",
            "alterar",
            "implemente",
            "implementar",
            "processe",
            "processar",
            "analisar",
            "analise",
            "verifique",
            "verificar",
            "corrija",
            "corrigir",
            "otimize",
            "otimizar",
            "refatore",
            "refatorar",
            "documente",
            "documentar",
            "escreva",
            "escrever",
            "salve",
            "salvar",
            "envie",
            "enviar",
            "liste",
            "listar",
            "mostre",
            "mostrar",
            "exiba",
            "exibir",
            "imprima",
            "imprimir",
            "calcule",
            "calcular",
            "soma",
            "somar",
            "conte",
            "contar",
            "filtre",
            "filtrar",
            "ordene",
            "ordenar",
            "agrupar",
            "busque",
            "buscar",
            "procure",
            "procurar",
            "consulte",
            "consultar",
            "instale",
            "instalar",
            "configure",
            "configurar",
            "inicialize",
            "inicializar",
            "atualize",
            "atualizar",
            "delete",
            "deletar",
            "remova",
            "remover",
            "converta",
            "converter",
            "exporte",
            "exportar",
            "importe",
            "importar",
            "valide",
            "validar",
            "check",
            "build",
            "compile",
            "compilar",
            "deploy",
        ];
        if let Some(first_word) = words.first() {
            if imperative_verbs.contains(first_word) {
                task_score += 0.35;
                reasons.push(format!("imperative:{}", first_word));
            }
        }

        // 7. Tamanho curto sem verbos de ação → conversacional
        if word_count < 5 {
            let has_action_verb = words
                .iter()
                .any(|w| imperative_verbs.contains(w) || task_keywords.contains(w));
            if !has_action_verb {
                conv_score += 0.30;
                reasons.push("short_no_action".to_string());
            }
        }

        // 8. Perguntas (interrogativas) → conversacional (a menos que seja "como faço para...")
        if normalized.ends_with('?') {
            conv_score += 0.20;
            reasons.push("question_mark".to_string());
        }
        let question_starters = [
            "o que", "oque", "qual", "quais", "quem", "quando", "onde", "como", "por que",
            "porque", "porquê",
        ];
        for starter in &question_starters {
            if normalized.starts_with(*starter) {
                conv_score += 0.25;
                reasons.push(format!("question_starter:{}", starter));
                // Mas "como faço para criar..." é híbrido
                if normalized.contains("faço")
                    || normalized.contains("fazer")
                    || normalized.contains("faz")
                {
                    if task_score > 0.3 {
                        reasons.push("how_to_do".to_string());
                    }
                }
            }
        }

        // ── Decisão final ────────────────────────────────────────────────────
        let total_score = task_score + conv_score;
        let task_ratio = if total_score > 0.0 {
            task_score / total_score
        } else {
            0.5
        };

        let (intent, confidence) = if task_ratio >= 0.7 {
            (UserIntent::Task, task_ratio)
        } else if task_ratio <= 0.3 {
            (UserIntent::Conversational, 1.0 - task_ratio)
        } else {
            (UserIntent::Hybrid, 0.5 + (task_ratio - 0.5).abs())
        };

        // Se nenhum sinal forte, default para Conversacional (safe default)
        let (intent, confidence) = if total_score < 0.1 {
            (UserIntent::Conversational, 0.6)
        } else {
            (intent, confidence)
        };

        let reason = if reasons.is_empty() {
            "default_conversational".to_string()
        } else {
            reasons.join(", ")
        };

        IntentResult {
            intent,
            confidence: confidence.min(1.0),
            reason,
        }
    }

    /// Retorna true se a intenção é definitivamente uma task (alta confiança).
    pub fn is_definitely_task(&self, input: &str) -> bool {
        let result = self.classify(input);
        result.intent == UserIntent::Task && result.confidence >= 0.7
    }

    /// Retorna true se a intenção é definitivamente conversacional.
    pub fn is_definitely_conversational(&self, input: &str) -> bool {
        let result = self.classify(input);
        result.intent == UserIntent::Conversational && result.confidence >= 0.7
    }
}

impl Default for IntentClassifier {
    fn default() -> Self {
        Self::new()
    }
}

// ── Testes ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn classifier() -> IntentClassifier {
        IntentClassifier::new()
    }

    #[test]
    fn saudacao_e_conversacional() {
        let c = classifier();
        let r = c.classify("Bom dia!");
        assert_eq!(r.intent, UserIntent::Conversational);
        assert!(r.confidence >= 0.5);
    }

    #[test]
    fn pergunta_simples_e_conversacional() {
        let c = classifier();
        let r = c.classify("O que é Rust?");
        assert_eq!(r.intent, UserIntent::Conversational);
    }

    #[test]
    fn criar_arquivo_e_task() {
        let c = classifier();
        let r = c.classify("Crie um arquivo hello.rs");
        assert_eq!(r.intent, UserIntent::Task);
        assert!(r.confidence >= 0.6);
    }

    #[test]
    fn gerar_planilha_e_task() {
        let c = classifier();
        let r = c.classify("Gere uma planilha de estoque");
        assert_eq!(r.intent, UserIntent::Task);
    }

    #[test]
    fn como_fazer_para_criar_e_hybrid() {
        let c = classifier();
        let r = c.classify("Como faço para criar uma função?");
        // "Como faço para" é question starter (+0.25) + "criar" é task keyword (+0.25)
        // Deve ser híbrido: tem pergunta E ação
        assert_eq!(
            r.intent,
            UserIntent::Hybrid,
            "Esperado Hybrid, got {:?} (reason: {})",
            r.intent,
            r.reason
        );
    }

    #[test]
    fn agradecimento_e_conversacional() {
        let c = classifier();
        let r = c.classify("Obrigado, isso resolveu!");
        assert_eq!(r.intent, UserIntent::Conversational);
    }

    #[test]
    fn executar_comando_e_task() {
        let c = classifier();
        let r = c.classify("Execute cargo test");
        assert_eq!(r.intent, UserIntent::Task);
    }

    #[test]
    fn mensagem_curta_sem_acao_e_conversacional() {
        let c = classifier();
        let r = c.classify("Ok");
        assert_eq!(r.intent, UserIntent::Conversational);
    }

    #[test]
    fn implementar_funcao_e_task() {
        let c = classifier();
        let r = c.classify("Implemente uma função de ordenação");
        assert_eq!(r.intent, UserIntent::Task);
    }

    #[test]
    fn explicar_algo_e_conversacional() {
        let c = classifier();
        let r = c.classify("Explique como funciona o borrow checker");
        assert_eq!(r.intent, UserIntent::Conversational);
    }

    #[test]
    fn is_definitely_task_funciona() {
        let c = classifier();
        assert!(c.is_definitely_task("Crie um arquivo main.rs"));
        assert!(!c.is_definitely_task("O que é Rust?"));
    }

    #[test]
    fn planilha_estoque_e_task() {
        let c = classifier();
        let r =
            c.classify("Preciso de uma planilha de controle de estoque para minha loja de roupas");
        assert_eq!(r.intent, UserIntent::Task);
    }
}
