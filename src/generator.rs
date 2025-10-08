use super::*;

struct Generator {
    config: Arc<PoolConfig>,
}

impl Generator {
    pub(crate) fn new(config: Arc<PoolConfig>) -> Self {
        Self {
            config
        }
    }

    pub(crate) fn get_block_template(&self) -> Result<BlockTemplate> {
        let mut rules = vec!["segwit"];
        if self.config.chain().network() == Network::Signet {
            rules.push("signet");
        }

        let params = json!({
            "capabilities": ["coinbasetxn", "workid", "coinbase/append"],
            "rules": rules,
        });

        Ok(self
            .config
            .bitcoin_rpc_client()?
            .call::<BlockTemplate>("getblocktemplate", &[params])?)
    }
}

