//! Test support: a fake provider, the provider conformance suite, a recording command runner
//! and the golden-output harness.

pub mod conformance;
mod fake;
mod golden;
mod recording;

pub use conformance::{ConformanceFixture, Failure};
pub use fake::FakeProvider;
pub use golden::{Golden, RunOutput};
pub use recording::RecordingRunner;
