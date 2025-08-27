use super::*;
use std::collections::HashMap;

pub(crate) struct CommandBuilder {
    args: Vec<String>,
    integration_test: bool,
    stderr: bool,
    stdin: Vec<u8>,
    stdout: bool,
    tempdir: Arc<TempDir>,
    environment: HashMap<String, String>,
}

impl CommandBuilder {
    pub(crate) fn new(args: impl ToArgs) -> Self {
        Self {
            args: args.to_args(),
            integration_test: true,
            stderr: true,
            stdin: Vec::new(),
            stdout: true,
            tempdir: Arc::new(TempDir::new().unwrap()),
            environment: HashMap::new(),
        }
    }

    pub(crate) fn integration_test(self, integration_test: bool) -> Self {
        Self {
            integration_test,
            ..self
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn with_loglevel(mut self, level: String) -> Self {
        self.environment.insert("RUST_LOG".into(), level);
        self
    }

    #[allow(unused)]
    pub(crate) fn capture_stderr(self, stderr: bool) -> Self {
        Self { stderr, ..self }
    }

    #[allow(unused)]
    pub(crate) fn capture_stdout(self, stdout: bool) -> Self {
        Self { stdout, ..self }
    }

    pub(crate) fn command(&self) -> Command {
        let mut command = Command::new(executable_path("para"));

        let mut args = Vec::new();

        for arg in self.args.iter() {
            args.push(arg.clone());
        }

        if self.integration_test {
            command.env("PARA_INTEGRATION_TEST", "1");
        }

        command
            .stdin(Stdio::piped())
            .stdout(if self.stdout {
                Stdio::piped()
            } else {
                Stdio::inherit()
            })
            .stderr(if self.stderr {
                Stdio::piped()
            } else {
                Stdio::inherit()
            })
            .current_dir(&*self.tempdir)
            .args(&args);

        command
    }

    #[track_caller]
    pub(crate) fn spawn(self) -> Child {
        let mut command = self.command();

        for (key, val) in self.environment.iter() {
            command.env(key, val);
        }

        let child = command.spawn().unwrap();

        child
            .stdin
            .as_ref()
            .unwrap()
            .write_all(&self.stdin)
            .unwrap();

        child
    }
}
