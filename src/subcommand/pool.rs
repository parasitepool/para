use {super::*, config::PoolConfig};

mod config;

#[derive(Parser, Debug)]
pub(crate) struct Pool {
    #[command(flatten)]
    pub(crate) config: PoolConfig,
}

impl Pool {
    pub(crate) fn run(&self) -> Result {
        let client = self.config.bitcoin_rpc_client()?;

        println!("{:?}", client.get_blockchain_info());

        // println!("{:?}", client.get_block_template(mode, rules, capabilities));
        
        Ok(())
    }
}
