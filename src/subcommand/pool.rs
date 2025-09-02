use {super::*, pool_config::PoolConfig};

pub(crate) mod pool_config;

#[derive(Parser, Debug)]
pub(crate) struct Pool {
    #[command(flatten)]
    pub(crate) config: PoolConfig,
}

impl Pool {
    pub(crate) async fn run(&self) -> Result {
        let config = Arc::new(self.config.clone());
        let address = config.address();
        let port = config.port();

        let listener = TcpListener::bind((address.clone(), port)).await?;

        info!("Listening on {address}:{port}");

        loop {
            tokio::select! {
                _ = Self::handle_single_user(config.clone(), &listener) => {}
                _ = ctrl_c() => {
                        info!("Shutting down stratum server");
                        break;
                    }
            }
        }

        Ok(())
    }

    async fn handle_single_user(config: Arc<PoolConfig>, listener: &TcpListener) -> Result {
        let (stream, peer) = listener.accept().await?;

        info!("Accepted connection from {peer}");

        let (reader, writer) = {
            let (rx, tx) = stream.into_split();
            (BufReader::new(rx), BufWriter::new(tx))
        };

        let mut conn = Connection::new(config.clone(), peer, reader, writer);

        conn.serve().await
    }
}
