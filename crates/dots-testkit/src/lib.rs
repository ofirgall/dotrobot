//! Test support: a fake provider, the provider conformance suite, a recording command runner
//! and the golden-output harness.

pub mod conformance;
mod fake;
mod golden;
mod recording;
mod sys;

pub use conformance::{ConformanceFixture, Failure};
pub use fake::FakeProvider;
pub use golden::{Golden, RunOutput};
pub use recording::RecordingRunner;
pub use sys::{PATH_SEP, bash_on_windows, canonical, path_with, system_env, system_path};
