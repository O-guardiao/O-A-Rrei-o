use anyhow::{bail, Result};
use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Status de um todo item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

/// Item de tarefa no Kanban/Todo panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: TodoStatus,
    pub created_at: u64,
    pub updated_at: u64,
}

impl TodoItem {
    pub fn new(id: impl Into<String>, content: impl Into<String>) -> Self {
        let now = now_epoch_secs();
        Self {
            id: id.into(),
            content: content.into(),
            status: TodoStatus::Pending,
            created_at: now,
            updated_at: now,
        }
    }
}

/// Store de todos persistido no Blackboard.
pub struct TodoStore {
    blackboard: Blackboard,
    session_id: String,
}

impl TodoStore {
    pub fn new(blackboard: Blackboard, session_id: impl Into<String>) -> Self {
        Self {
            blackboard,
            session_id: session_id.into(),
        }
    }

    fn bb_key(&self) -> String {
        format!("todos:{}", self.session_id)
    }

    pub fn create(&self, id: impl Into<String>, content: impl Into<String>) -> Result<TodoItem> {
        let item = TodoItem::new(id, content);
        let mut items = self.load_items()?;
        if items.contains_key(&item.id) {
            bail!("todo com id '{}' já existe", item.id);
        }
        items.insert(item.id.clone(), item.clone());
        self.save_items(&items)?;
        Ok(item)
    }

    pub fn update(
        &self,
        id: &str,
        content: Option<String>,
        status: Option<TodoStatus>,
    ) -> Result<TodoItem> {
        let mut items = self.load_items()?;
        let item = items
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("todo '{}' não encontrado", id))?;
        if let Some(c) = content {
            item.content = c;
        }
        if let Some(s) = status {
            item.status = s;
        }
        item.updated_at = now_epoch_secs();
        let updated = item.clone();
        self.save_items(&items)?;
        Ok(updated)
    }

    pub fn complete(&self, id: &str) -> Result<TodoItem> {
        self.update(id, None, Some(TodoStatus::Completed))
    }

    pub fn cancel(&self, id: &str) -> Result<TodoItem> {
        self.update(id, None, Some(TodoStatus::Cancelled))
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let mut items = self.load_items()?;
        if items.remove(id).is_none() {
            bail!("todo '{}' não encontrado", id);
        }
        self.save_items(&items)
    }

    pub fn get(&self, id: &str) -> Option<TodoItem> {
        self.load_items().ok()?.get(id).cloned()
    }

    pub fn list(&self) -> Vec<TodoItem> {
        self.load_items()
            .unwrap_or_default()
            .values()
            .cloned()
            .collect()
    }

    pub fn list_by_status(&self, status: TodoStatus) -> Vec<TodoItem> {
        self.list()
            .into_iter()
            .filter(|t| t.status == status)
            .collect()
    }

    /// Merge: atualiza existentes e adiciona novos.
    pub fn merge(&self, new_items: Vec<TodoItem>) -> Result<()> {
        let mut items = self.load_items()?;
        for item in new_items {
            items.insert(item.id.clone(), item);
        }
        self.save_items(&items)
    }

    /// Trail: retorna todos concluídos/cancelados para arquivamento.
    pub fn trail(&self) -> Vec<TodoItem> {
        self.list()
            .into_iter()
            .filter(|t| matches!(t.status, TodoStatus::Completed | TodoStatus::Cancelled))
            .collect()
    }

    /// Limpa todos concluídos/cancelados (após arquivar no transcript).
    pub fn clear_trail(&self) -> Result<usize> {
        let mut items = self.load_items()?;
        let before = items.len();
        items.retain(|_, v| !matches!(v.status, TodoStatus::Completed | TodoStatus::Cancelled));
        let after = items.len();
        self.save_items(&items)?;
        Ok(before - after)
    }

    /// Kanban summary: (pending, in_progress, completed, cancelled, total)
    pub fn kanban_summary(&self) -> (usize, usize, usize, usize, usize) {
        let items = self.list();
        let pending = items
            .iter()
            .filter(|t| t.status == TodoStatus::Pending)
            .count();
        let in_progress = items
            .iter()
            .filter(|t| t.status == TodoStatus::InProgress)
            .count();
        let completed = items
            .iter()
            .filter(|t| t.status == TodoStatus::Completed)
            .count();
        let cancelled = items
            .iter()
            .filter(|t| t.status == TodoStatus::Cancelled)
            .count();
        (pending, in_progress, completed, cancelled, items.len())
    }

    fn load_items(&self) -> Result<HashMap<String, TodoItem>> {
        match self.blackboard.get_tuple("todos", &self.bb_key()) {
            Some(v) => Ok(serde_json::from_value(v)?),
            None => Ok(HashMap::new()),
        }
    }

    fn save_items(&self, items: &HashMap<String, TodoItem>) -> Result<()> {
        let value = serde_json::to_value(items)?;
        self.blackboard.put_tuple("todos", &self.bb_key(), value)
    }
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_store() -> TodoStore {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        let bb = Blackboard::open(&p).unwrap();
        TodoStore::new(bb, "test-session")
    }

    #[test]
    fn create_and_get_todo() {
        let store = temp_store();
        let item = store.create("t1", "Build API").unwrap();
        assert_eq!(item.content, "Build API");
        assert!(matches!(item.status, TodoStatus::Pending));

        let retrieved = store.get("t1").unwrap();
        assert_eq!(retrieved.content, "Build API");
    }

    #[test]
    fn update_status() {
        let store = temp_store();
        store.create("t1", "Task 1").unwrap();
        let updated = store
            .update("t1", None, Some(TodoStatus::InProgress))
            .unwrap();
        assert!(matches!(updated.status, TodoStatus::InProgress));
    }

    #[test]
    fn complete_and_cancel() {
        let store = temp_store();
        store.create("t1", "Task 1").unwrap();
        store.create("t2", "Task 2").unwrap();

        let done = store.complete("t1").unwrap();
        assert!(matches!(done.status, TodoStatus::Completed));

        let cancelled = store.cancel("t2").unwrap();
        assert!(matches!(cancelled.status, TodoStatus::Cancelled));
    }

    #[test]
    fn list_by_status() {
        let store = temp_store();
        store.create("t1", "Pending task").unwrap();
        store.create("t2", "In progress task").unwrap();
        store
            .update("t2", None, Some(TodoStatus::InProgress))
            .unwrap();

        let pending = store.list_by_status(TodoStatus::Pending);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "t1");

        let in_progress = store.list_by_status(TodoStatus::InProgress);
        assert_eq!(in_progress.len(), 1);
        assert_eq!(in_progress[0].id, "t2");
    }

    #[test]
    fn merge_updates_existing() {
        let store = temp_store();
        store.create("t1", "Old content").unwrap();

        let merged = TodoItem {
            id: "t1".to_string(),
            content: "New content".to_string(),
            status: TodoStatus::InProgress,
            created_at: 0,
            updated_at: 0,
        };
        store.merge(vec![merged]).unwrap();

        let item = store.get("t1").unwrap();
        assert_eq!(item.content, "New content");
        assert!(matches!(item.status, TodoStatus::InProgress));
    }

    #[test]
    fn trail_and_clear() {
        let store = temp_store();
        store.create("t1", "Done").unwrap();
        store.create("t2", "Cancelled").unwrap();
        store.create("t3", "Pending").unwrap();
        store.complete("t1").unwrap();
        store.cancel("t2").unwrap();

        let trail = store.trail();
        assert_eq!(trail.len(), 2);

        let cleared = store.clear_trail().unwrap();
        assert_eq!(cleared, 2);
        assert_eq!(store.list().len(), 1);
    }

    #[test]
    fn kanban_summary() {
        let store = temp_store();
        store.create("t1", "P1").unwrap();
        store.create("t2", "P2").unwrap();
        store.create("t3", "IP").unwrap();
        store
            .update("t3", None, Some(TodoStatus::InProgress))
            .unwrap();
        store.create("t4", "Done").unwrap();
        store.complete("t4").unwrap();

        let (pending, in_progress, completed, cancelled, total) = store.kanban_summary();
        assert_eq!(pending, 2);
        assert_eq!(in_progress, 1);
        assert_eq!(completed, 1);
        assert_eq!(cancelled, 0);
        assert_eq!(total, 4);
    }

    #[test]
    fn duplicate_id_fails() {
        let store = temp_store();
        store.create("t1", "First").unwrap();
        assert!(store.create("t1", "Second").is_err());
    }

    #[test]
    fn remove_todo() {
        let store = temp_store();
        store.create("t1", "To remove").unwrap();
        store.remove("t1").unwrap();
        assert!(store.get("t1").is_none());
    }
}
