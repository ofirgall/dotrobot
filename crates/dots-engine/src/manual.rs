//! Per-machine record of manual actions (`packages/<pkg>/manual/*.md`, spec `manual-actions`).
//!
//! The record lives in `state.json` (`manual`, keyed by the full id `<pkg>/<id>`), so what one
//! machine has done says nothing about another. An action with no record, or whose record is for
//! an older `revision`, is pending: bumping the revision in the repo reopens it everywhere,
//! including where it was done or skipped. Nothing here runs a check; the CLI does, and writes
//! the outcome with [`State::close_manual`].

use serde::{Deserialize, Serialize};

use crate::state::State;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ManualStatus {
    Pending,
    Done,
    Skipped,
    /// Closed because its `check` passed.
    Auto,
}

impl ManualStatus {
    pub fn word(self) -> &'static str {
        match self {
            ManualStatus::Pending => "pending",
            ManualStatus::Done => "done",
            ManualStatus::Skipped => "skipped",
            ManualStatus::Auto => "auto",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualRecord {
    /// The full id, `<pkg>/<id>`.
    pub id: String,
    pub revision: u32,
    pub status: ManualStatus,
    pub at: String,
    /// The repo commit the action was closed against, when the repo is a checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_rev: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl State {
    /// Where the action stands on this machine: its record counts only for the revision it was
    /// made against or a later one.
    pub fn manual_status(&self, full_id: &str, revision: u32) -> ManualStatus {
        match self.manual.get(full_id) {
            Some(r) if r.revision >= revision => r.status,
            _ => ManualStatus::Pending,
        }
    }

    /// Record `status` for the action at `revision`.
    pub fn close_manual(
        &mut self,
        full_id: &str,
        revision: u32,
        status: ManualStatus,
        at: String,
        repo_rev: Option<String>,
        note: Option<String>,
    ) {
        self.manual.insert(
            full_id.to_string(),
            ManualRecord {
                id: full_id.to_string(),
                revision,
                status,
                at,
                repo_rev,
                note,
            },
        );
    }

    /// Forget the record: the action is pending again.
    pub fn reopen_manual(&mut self, full_id: &str) -> bool {
        self.manual.remove(full_id).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_newer_revision_reopens_every_closed_status() {
        for s in [
            ManualStatus::Done,
            ManualStatus::Skipped,
            ManualStatus::Auto,
        ] {
            let mut st = State::default();
            st.close_manual("k/p", 1, s, "t".into(), None, None);
            assert_eq!(st.manual_status("k/p", 1), s);
            assert_eq!(st.manual_status("k/p", 2), ManualStatus::Pending);
        }
        assert_eq!(
            State::default().manual_status("k/p", 1),
            ManualStatus::Pending
        );
    }

    #[test]
    fn records_survive_a_round_trip() {
        let mut st = State::default();
        let (rev, note) = (Some("abc".to_string()), Some("n".to_string()));
        st.close_manual("k/p", 3, ManualStatus::Done, "t".into(), rev, note);
        let text = serde_json::to_string(&st).unwrap();
        let back: State = serde_json::from_str(&text).unwrap();
        assert_eq!(back.manual["k/p"].note.as_deref(), Some("n"));
        assert_eq!(back.manual["k/p"].repo_rev.as_deref(), Some("abc"));
    }
}
