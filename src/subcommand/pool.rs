use {super::*, pool_config::PoolConfig};

mod pool_config;

#[derive(Parser, Debug)]
pub(crate) struct Pool {
    #[command(flatten)]
    pub(crate) config: PoolConfig,
}

impl Pool {
    pub(crate) async fn run(&self) -> Result {
        let config = &self.config;
        let client = config.bitcoin_rpc_client()?;

        client.get_blockchain_info()?;
        // println!("{:?}", client.get_block_template(mode, rules, capabilities));

        let address = config.address();
        let port = config.port();

        let listener = TcpListener::bind((address.clone(), port)).await?;

        info!("Listening on {address}:{port}");

        let (stream, miner) = listener.accept().await?;

        info!("Accepted connection from {miner}");

        let reader = BufReader::new(stream);
        let mut lines = reader.lines();

        while let Some(line) = lines.next_line().await? {
            println!("> {line}");
        }

        info!("Connection closed by {miner}");

        Ok(())
    }
}
