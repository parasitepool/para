use super::*;

#[derive(Clone, Default, Debug, Parser)]
pub struct Options {
    #[arg(long, help = "Directory where ckpool writes its logs.")]
    pub(crate) log_dir: Option<PathBuf>,
}

impl Options {
    pub(crate) fn log_dir(&self) -> PathBuf {
        self.log_dir.clone().unwrap_or_else(|| {
            std::env::current_dir().expect("Failed to get current working directory")
        })
    }
}
