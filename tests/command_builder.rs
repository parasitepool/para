use super::*;

pub(crate) trait ToArgs {
    fn to_args(&self) -> Vec<String>;
}

impl ToArgs for String {
    fn to_args(&self) -> Vec<String> {
        self.as_str().to_args()
    }
}

impl ToArgs for &str {
    fn to_args(&self) -> Vec<String> {
        self.split_whitespace().map(str::to_string).collect()
    }
}

impl<const N: usize> ToArgs for [&str; N] {
    fn to_args(&self) -> Vec<String> {
        self.iter().cloned().map(str::to_string).collect()
    }
}

impl ToArgs for Vec<String> {
    fn to_args(&self) -> Vec<String> {
        self.clone()
    }
}

pub(crate) struct CommandBuilder {
    args: Vec<String>,
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
    pub(crate) fn write(self, path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> Self {
        fs::write(self.tempdir.path().join(path), contents).unwrap();
        self
    }

    #[allow(unused)]
    pub(crate) fn stderr(self, stderr: bool) -> Self {
        Self { stderr, ..self }
    }

    #[allow(unused)]
    pub(crate) fn stdout(self, stdout: bool) -> Self {
        Self { stdout, ..self }
    }

    pub(crate) fn command(&self) -> Command {
        let mut command = Command::new(executable_path("para"));

        let mut args = Vec::new();

        for arg in self.args.iter() {
            args.push(arg.clone());
            if arg == "server" {
                args.push("--log-dir".to_string());
                args.push(self.tempdir.path().display().to_string());
            }
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
