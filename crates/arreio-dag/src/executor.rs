use crate::{Dag, DagNode, NodeStatus};
use anyhow::Result;
use crossbeam_channel::{unbounded, Receiver, Sender};
use std::thread;

/// Resultado da execução de um nó pelo worker.
pub struct NodeResult {
    pub node_id: String,
    pub success: bool,
}

/// Executor paralelo do DAG usando thread pool síncrona.
pub struct DagExecutor {
    workers: usize,
}

impl DagExecutor {
    pub fn new(workers: usize) -> Self {
        Self { workers }
    }

    /// Executa o DAG em paralelo.
    /// A closure `exec` recebe um nó e deve retornar `Ok(())` em sucesso ou `Err` em falha.
    /// A closure é chamada em threads worker; deve ser `Send + Sync`.
    pub fn run<F>(&self, dag: &mut Dag, exec: F) -> Result<()>
    where
        F: Fn(&DagNode) -> Result<()> + Send + Sync + 'static,
    {
        let (task_tx, task_rx): (Sender<DagNode>, Receiver<DagNode>) = unbounded();
        let (result_tx, result_rx): (Sender<NodeResult>, Receiver<NodeResult>) = unbounded();

        // Compartilha a closure entre workers via Arc
        let exec = std::sync::Arc::new(exec);

        // Spawn workers
        let mut handles = Vec::with_capacity(self.workers);
        for _ in 0..self.workers {
            let rx = task_rx.clone();
            let tx = result_tx.clone();
            let f = exec.clone();
            let handle = thread::spawn(move || {
                while let Ok(node) = rx.recv() {
                    let success = f(&node).is_ok();
                    let _ = tx.send(NodeResult {
                        node_id: node.id,
                        success,
                    });
                }
            });
            handles.push(handle);
        }
        // Descarta o clone original do receiver para workers terminarem quando vazio
        drop(task_rx);
        drop(result_tx);

        // Envia nós iniciais prontos, em ordem de prioridade (PVC-Q3.1):
        // score composto decrescente; nós sem score usam o default neutro.
        let now = crate::score::now_epoch_secs();
        let pending: Vec<DagNode> = dag
            .scored_ready_nodes(now)
            .iter()
            .map(|(n, _)| (*n).clone())
            .collect();
        for node in &pending {
            dag.update_status(&node.id, NodeStatus::Running)?;
            let _ = task_tx.send(node.clone());
        }

        // Coordena resultados e agenda novos nós
        let mut completed = 0usize;
        let total = dag.nodes().len();
        let mut task_tx_opt = Some(task_tx);

        while completed < total {
            // Verifica se há trabalho em andamento ou pendente
            let running_count = dag
                .nodes()
                .iter()
                .filter(|n| n.status == NodeStatus::Running)
                .count();
            let ready_count = dag.ready_nodes().len();

            if running_count == 0 && ready_count == 0 {
                // Nenhum nó pode mais avançar (deadlock por falha ou ciclo impossível)
                // Sinaliza workers para terminarem
                drop(task_tx_opt.take());
                // Consome resultados pendentes (se houver)
                while let Ok(res) = result_rx.recv() {
                    let status = if res.success {
                        NodeStatus::Success
                    } else {
                        NodeStatus::Failed
                    };
                    dag.update_status(&res.node_id, status)?;
                }
                break;
            }

            // Se não há workers ativos e não há resultados, quebra
            if result_rx.is_empty() && handles.iter().all(|h| h.is_finished()) {
                drop(task_tx_opt.take());
                break;
            }

            if let Ok(res) = result_rx.recv() {
                completed += 1;
                let status = if res.success {
                    NodeStatus::Success
                } else {
                    NodeStatus::Failed
                };
                dag.update_status(&res.node_id, status)?;

                // Envia novos nós que ficaram prontos, priorizados por score
                // (re-scoring dinâmico: o score é relido a cada ciclo).
                let now = crate::score::now_epoch_secs();
                let new_ready: Vec<DagNode> = dag
                    .scored_ready_nodes(now)
                    .iter()
                    .map(|(n, _)| (*n).clone())
                    .collect();
                if let Some(ref tx) = task_tx_opt {
                    for node in new_ready {
                        dag.update_status(&node.id, NodeStatus::Running)?;
                        let _ = tx.send(node);
                    }
                }
            }
        }

        // Garante que task_tx está dropado
        drop(task_tx_opt);

        // Aguarda workers terminarem
        for h in handles {
            let _ = h.join();
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DagNode, NodeStatus};
    use arreio_kernel::Blackboard;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tempfile::NamedTempFile;

    fn temp_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&p).unwrap()
    }

    fn make_node(id: &str, deps: Vec<&str>) -> DagNode {
        DagNode {
            id: id.to_string(),
            title: id.to_string(),
            depends_on: deps.into_iter().map(String::from).collect(),
            status: NodeStatus::Waiting,
            actor_type: "developer".to_string(),
            file_target: None,
            instruction: String::new(),
            payload: serde_json::Value::Null,
            validation_cmd: None,
            acceptance_criteria: vec![],
            decision_log: vec![],
            assigned_agent: None,
            retry_count: 0,
            contracts: vec![],
        }
    }

    #[test]
    fn executor_paralelo_completa_dag() {
        let nodes = vec![
            make_node("a", vec![]),
            make_node("b", vec![]),
            make_node("c", vec!["a", "b"]),
        ];
        let mut dag = Dag::new(nodes, temp_bb()).unwrap();
        let exec_count = Arc::new(AtomicUsize::new(0));
        let exec_count_clone = exec_count.clone();

        let executor = DagExecutor::new(2);
        executor
            .run(&mut dag, move |_| {
                exec_count_clone.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .unwrap();

        assert!(dag.is_complete());
        assert_eq!(exec_count.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn executor_prioriza_por_score_com_worker_unico() {
        use crate::NodeScore;
        use std::sync::Mutex;

        let nodes = vec![
            make_node("c-baixo", vec![]),
            make_node("a-alto", vec![]),
            make_node("b-medio", vec![]),
        ];
        let dag_bb = temp_bb();
        let mut dag = Dag::new(nodes, dag_bb).unwrap();
        dag.set_score("a-alto", &NodeScore::new(1.0, 1.0, 0.0, 0.0))
            .unwrap();
        dag.set_score("b-medio", &NodeScore::new(0.5, 0.5, 0.0, 0.5))
            .unwrap();
        dag.set_score("c-baixo", &NodeScore::new(0.0, 0.0, 0.0, 1.0))
            .unwrap();

        let order = Arc::new(Mutex::new(Vec::<String>::new()));
        let order_clone = order.clone();

        // Worker único: a ordem de execução é a ordem de envio ao canal.
        let executor = DagExecutor::new(1);
        executor
            .run(&mut dag, move |node| {
                order_clone.lock().unwrap().push(node.id.clone());
                Ok(())
            })
            .unwrap();

        assert!(dag.is_complete());
        let observed = order.lock().unwrap().clone();
        assert_eq!(observed, vec!["a-alto", "b-medio", "c-baixo"]);
    }

    #[test]
    fn executor_marca_falha() {
        let nodes = vec![make_node("a", vec![]), make_node("b", vec!["a"])];
        let mut dag = Dag::new(nodes, temp_bb()).unwrap();

        let executor = DagExecutor::new(1);
        executor
            .run(&mut dag, |node| {
                if node.id == "a" {
                    Err(anyhow::anyhow!("falha forçada"))
                } else {
                    Ok(())
                }
            })
            .unwrap();

        let a = dag.nodes().iter().find(|n| n.id == "a").unwrap();
        let b = dag.nodes().iter().find(|n| n.id == "b").unwrap();
        assert_eq!(a.status, NodeStatus::Failed);
        // b nunca deve ter rodado porque depende de a
        assert_eq!(b.status, NodeStatus::Waiting);
    }
}
