use super::*;

#[derive(Debug, Parser)]
pub(crate) struct Db {
    #[command(subcommand)]
    subcommand: Subcommand,
}

#[derive(Debug, clap::Subcommand)]
enum Subcommand {
    #[command(about = "Create a consistent compacted backup of a redb database")]
    Backup {
        #[arg(long, help = "Path to source redb database <SOURCE>.")]
        source: PathBuf,
        #[arg(long, help = "Path to write backup database <DESTINATION>.")]
        destination: PathBuf,
    },
}

impl Db {
    pub(crate) async fn run(self, cancel_token: CancellationToken) -> Result {
        match self.subcommand {
            Subcommand::Backup {
                source,
                destination,
            } => {
                task::spawn_blocking(move || Store::backup(&source, &destination, &cancel_token))
                    .await?
            }
        }
    }
}
