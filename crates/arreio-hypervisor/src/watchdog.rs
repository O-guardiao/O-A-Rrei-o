use arreio_kernel::Blackboard;
use std::collections::VecDeque;

/// Detecta loops de alucinação: se o mesmo exit_code ocorrer N vezes
/// consecutivas, publica um evento de INTERRUPÇÃO no Blackboard.
/// Inspirado nas Interrupções de Hardware do spec.
pub struct Watchdog {
    history: VecDeque<i32>,
    max_repeats: usize,
    blackboard: Blackboard,
}

impl Watchdog {
    pub fn new(max_repeats: usize, blackboard: Blackboard) -> Self {
        Self {
            history: VecDeque::with_capacity(max_repeats + 1),
            max_repeats,
            blackboard,
        }
    }

    /// Registra o exit_code de uma execução.
    /// Retorna `true` se o interrupt foi disparado.
    pub fn record(&mut self, exit_code: i32) -> bool {
        self.history.push_back(exit_code);
        if self.history.len() > self.max_repeats {
            self.history.pop_front();
        }

        if self.should_interrupt() {
            let _ = self.blackboard.publish(
                "interrupt",
                serde_json::json!({
                    "reason":    "loop_detectado",
                    "exit_code": exit_code,
                    "repeats":   self.max_repeats
                }),
            );
            self.history.clear();
            return true;
        }
        false
    }

    fn should_interrupt(&self) -> bool {
        if self.history.len() < self.max_repeats {
            return false;
        }
        let first = self.history[0];
        self.history.iter().all(|&c| c == first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_watchdog(n: usize) -> Watchdog {
        let f = NamedTempFile::new().unwrap();
        let path: PathBuf = f.path().to_path_buf();
        drop(f);
        let bb = Blackboard::open(&path).unwrap();
        Watchdog::new(n, bb)
    }

    #[test]
    fn no_interrupt_on_varied_codes() {
        let mut w = temp_watchdog(3);
        assert!(!w.record(1));
        assert!(!w.record(2));
        assert!(!w.record(1));
    }

    #[test]
    fn interrupt_on_three_same_codes() {
        let mut w = temp_watchdog(3);
        assert!(!w.record(1));
        assert!(!w.record(1));
        assert!(w.record(1)); // 3ª ocorrência → interrupt
    }

    #[test]
    fn resets_after_interrupt() {
        let mut w = temp_watchdog(3);
        w.record(1);
        w.record(1);
        w.record(1); // dispara
                     // após reset interno, não deve disparar imediatamente
        assert!(!w.record(1));
        assert!(!w.record(1));
    }

    #[test]
    fn publishes_event_to_blackboard() {
        let f = NamedTempFile::new().unwrap();
        let path: PathBuf = f.path().to_path_buf();
        drop(f);
        let bb = Blackboard::open(&path).unwrap();
        let mut w = Watchdog::new(3, bb.clone());
        w.record(2);
        w.record(2);
        w.record(2);
        assert!(bb.has_event("interrupt"));
    }
}
