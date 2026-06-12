use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Evento de stream unificado.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StreamEvent {
    Ready,
    MessageStart { session_id: String },
    MessageDelta { content: String },
    MessageComplete,
    ToolStart { tool_name: String },
    ToolProgress { tool_name: String, message: String },
    ToolComplete { tool_name: String, result: String },
    ApprovalRequest { tool_name: String, command: String },
}

/// Consumer bufferizado que desacopla geração de tokens da entrega.
pub struct StreamConsumer {
    buffer: Arc<Mutex<VecDeque<StreamEvent>>>,
    max_size: usize,
}

impl StreamConsumer {
    pub fn new(max_size: usize) -> Self {
        Self {
            buffer: Arc::new(Mutex::new(VecDeque::new())),
            max_size,
        }
    }

    /// Enfileira um evento. Se o buffer estiver cheio, descarta o mais antigo.
    pub fn push(&self, event: StreamEvent) {
        let mut buf = self.buffer.lock().unwrap();
        if buf.len() >= self.max_size {
            buf.pop_front();
        }
        buf.push_back(event);
    }

    /// Consome o próximo evento (FIFO).
    pub fn next(&self) -> Option<StreamEvent> {
        self.buffer.lock().unwrap().pop_front()
    }

    /// Verifica se há eventos pendentes.
    pub fn has_pending(&self) -> bool {
        !self.buffer.lock().unwrap().is_empty()
    }

    /// Retorna o número de eventos pendentes.
    pub fn pending_count(&self) -> usize {
        self.buffer.lock().unwrap().len()
    }

    /// Limpa o buffer.
    pub fn clear(&self) {
        self.buffer.lock().unwrap().clear();
    }

    /// Snapshot atual do buffer (sem consumir).
    pub fn snapshot(&self) -> Vec<StreamEvent> {
        self.buffer.lock().unwrap().iter().cloned().collect()
    }
}

impl Default for StreamConsumer {
    fn default() -> Self {
        Self::new(1000)
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_next() {
        let consumer = StreamConsumer::new(10);
        consumer.push(StreamEvent::Ready);
        consumer.push(StreamEvent::MessageStart {
            session_id: "s1".to_string(),
        });

        assert_eq!(consumer.pending_count(), 2);
        assert!(matches!(consumer.next(), Some(StreamEvent::Ready)));
        assert_eq!(consumer.pending_count(), 1);
    }

    #[test]
    fn buffer_discards_oldest_when_full() {
        let consumer = StreamConsumer::new(3);
        consumer.push(StreamEvent::Ready);
        consumer.push(StreamEvent::MessageStart {
            session_id: "s1".to_string(),
        });
        consumer.push(StreamEvent::MessageStart {
            session_id: "s2".to_string(),
        });
        consumer.push(StreamEvent::MessageStart {
            session_id: "s3".to_string(),
        });

        assert_eq!(consumer.pending_count(), 3);
        // O primeiro (Ready) deve ter sido descartado
        let first = consumer.next().unwrap();
        assert!(matches!(first, StreamEvent::MessageStart { session_id } if session_id == "s1"));
    }

    #[test]
    fn snapshot_does_not_consume() {
        let consumer = StreamConsumer::new(10);
        consumer.push(StreamEvent::Ready);
        let snap = consumer.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(consumer.pending_count(), 1);
    }

    #[test]
    fn clear_empties_buffer() {
        let consumer = StreamConsumer::new(10);
        consumer.push(StreamEvent::Ready);
        consumer.clear();
        assert_eq!(consumer.pending_count(), 0);
        assert!(consumer.next().is_none());
    }
}
