use {super::*, crate::subcommand::server::database::Database};

#[derive(Debug, Parser)]
pub struct NodeToken {
    #[arg(
        long,
        help = "Connect to Postgres running at <DATABASE_URL>.",
        default_value = "postgres://satoshi:nakamoto@127.0.0.1:5432/ckpool"
    )]
    database_url: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, clap::Subcommand)]
enum Command {
    #[command(about = "Mint a token for <NAME>, replacing any existing one")]
    Mint {
        #[arg(long, help = "Hostname of the node the token is bound to.")]
        name: String,
    },
    #[command(about = "Revoke the token for <NAME>")]
    Revoke {
        #[arg(long, help = "Hostname of the node to revoke.")]
        name: String,
    },
    #[command(about = "List node tokens")]
    List,
}

impl NodeToken {
    pub(crate) async fn run(self) -> Result {
        let database = Database::new(self.database_url).await?;
        database.ensure_node_tokens_table().await?;

        match self.command {
            Command::Mint { name } => println!("{}", database.mint_node_token(&name).await?),
            Command::Revoke { name } => ensure!(
                database.revoke_node_token(&name).await?,
                "no active token for {name}"
            ),
            Command::List => {
                for token in database.list_node_tokens().await? {
                    println!(
                        "{}\tcreated {}\trevoked {}\tlast seen {}",
                        token.name,
                        token.created_at,
                        token.revoked_at.as_deref().unwrap_or("-"),
                        token.last_seen_at.as_deref().unwrap_or("-"),
                    );
                }
            }
        }

        Ok(())
    }
}
