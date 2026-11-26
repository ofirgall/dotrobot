//! Process exit codes (cli-surface "Exit codes", design D3).

/// The only exit codes `dots` ever returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    Success,
    Error,
    /// Drift present (`status`, `verify`).
    Drift,
    /// Non-empty plan (`plan --check`).
    PlanNonEmpty,
    /// Usage error (`EX_USAGE`).
    Usage,
    /// Another dots run holds the run lock (`EX_TEMPFAIL`).
    LockBusy,
    Interrupted,
}

impl ExitCode {
    pub const fn code(self) -> i32 {
        match self {
            ExitCode::Success => 0,
            ExitCode::Error => 1,
            ExitCode::Drift => 2,
            ExitCode::PlanNonEmpty => 3,
            ExitCode::Usage => 64,
            ExitCode::LockBusy => 75,
            ExitCode::Interrupted => 130,
        }
    }
}
