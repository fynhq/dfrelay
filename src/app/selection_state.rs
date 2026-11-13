//! Manual selection, selection generation and pending-start state.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PendingSelection {
    Auto,
    Node(String),
}

pub(crate) struct SelectionState {
    pub(crate) preferred_node: Option<String>,
    pub(crate) selection_generation: Arc<AtomicU64>,
    pub(crate) pending_selection: Option<PendingSelection>,
    pub(crate) pending_start: Option<u64>,
}

impl Default for SelectionState {
    fn default() -> Self {
        Self {
            preferred_node: None,
            selection_generation: Arc::new(AtomicU64::new(0)),
            pending_selection: None,
            pending_start: None,
        }
    }
}
