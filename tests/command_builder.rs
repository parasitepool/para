use super::*;

pub(crate) struct CommandBuilder {
    args: Vec<String>,
    env: BTreeMap<String, OsString>,
    integration_test: bool,
    stderr: bool,
    stdin: Vec<u8>,
    stdout: bool,
    tempdir: Arc<TempDir>,
}

impl CommandBuilder {
    pub(crate) fn new(args: impl ToArgs) -> Self {
        Self {
            args: args.to_args(),
            env: BTreeMap::new(),
            integration_test: true,
            stderr: true,
            stdin: Vec::new(),
            stdout: true,
            tempdir: Arc::new(TempDir::new().unwrap()),
        }
    }

    pub(crate) fn integration_test(self, integration_test: bool) -> Self {
        Self {
            integration_test,
            ..self
        }
    }

    #[allow(unused)]
    pub(crate) fn capture_stderr(self, stderr: bool) -> Self {
        Self { stderr, ..self }
    }

    #[allow(unused)]
    pub(crate) fn capture_stdout(self, stdout: bool) -> Self {
        Self { stdout, ..self }
    }

    #[allow(unused)]
    pub(crate) fn env(mut self, key: &str, value: impl AsRef<OsStr>) -> Self {
        self.env.insert(key.into(), value.as_ref().into());
        self
    }

    pub(crate) fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_para"));

        let mut args = Vec::new();

        for arg in self.args.iter() {
            args.push(arg.clone());
        }

        for (key, value) in &self.env {
            command.env(key, value);
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
