//! A command runner that records every call and can script outputs or delegate to a backend.

use std::sync::{Arc, Mutex};

use dots_providers::{Cmd, Output, Result, Runner};

#[derive(Debug)]
struct Rule {
    prefix: Vec<String>,
    output: Output,
}

/// Records every [`Cmd`] it is asked to run. Responses, in order of precedence:
/// scripted rules (`on`, argv-prefix match, first rule wins), then the wrapped backend, then an
/// empty successful output.
#[derive(Debug, Default)]
pub struct RecordingRunner {
    calls: Mutex<Vec<Cmd>>,
    rules: Mutex<Vec<Rule>>,
    backend: Option<Arc<dyn Runner>>,
}

impl RecordingRunner {
    /// Scripted only; unmatched commands succeed with empty output.
    pub fn new() -> Self {
        Self::default()
    }

    /// Delegates unscripted commands to `backend` (an in-memory fake, or `RealRunner` for
    /// real-backend conformance runs).
    pub fn wrapping(backend: Arc<dyn Runner>) -> Self {
        RecordingRunner {
            backend: Some(backend),
            ..Self::default()
        }
    }

    /// Script the output for any command whose argv (program, args...) starts with `prefix`.
    pub fn on(&self, prefix: &[&str], output: Output) -> &Self {
        self.rules.lock().unwrap().push(Rule {
            prefix: prefix.iter().map(|s| s.to_string()).collect(),
            output,
        });
        self
    }

    /// Every command run so far, in order.
    pub fn commands(&self) -> Vec<Cmd> {
        self.calls.lock().unwrap().clone()
    }

    /// Command lines (`program arg arg`), in order.
    pub fn lines(&self) -> Vec<String> {
        self.commands().iter().map(Cmd::line).collect()
    }

    pub fn count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    pub fn clear(&self) {
        self.calls.lock().unwrap().clear();
    }
}

impl Runner for RecordingRunner {
    fn run(&self, cmd: &Cmd) -> Result<Output> {
        self.calls.lock().unwrap().push(cmd.clone());
        let argv = cmd.argv();
        let scripted = self
            .rules
            .lock()
            .unwrap()
            .iter()
            .find(|r| argv.starts_with(&r.prefix))
            .map(|r| r.output.clone());
        if let Some(o) = scripted {
            return Ok(o);
        }
        match &self.backend {
            Some(b) => b.run(cmd),
            None => Ok(Output::default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_scripts() {
        let r = RecordingRunner::new();
        r.on(&["/bin/x", "list"], Output::ok("a\n"));
        assert_eq!(
            r.run(&Cmd::new("/bin/x").arg("list")).unwrap().stdout,
            "a\n"
        );
        assert!(r.run(&Cmd::new("/bin/y")).unwrap().success());
        assert_eq!(r.lines(), ["/bin/x list", "/bin/y"]);
        r.clear();
        assert_eq!(r.count(), 0);
    }
}
