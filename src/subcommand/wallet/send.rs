use super::*;

#[derive(Debug, Parser)]
pub(crate) struct Send {
    #[arg(long, help = "Recipient <ADDRESS>.")]
    address: Address<NetworkUnchecked>,
    #[arg(long, help = "<AMOUNT> in satoshis.")]
    amount: u64,
    #[arg(long, help = "Use fee rate of <FEE_RATE> sats/vB.")]
    fee_rate: u64,
}

#[derive(Serialize, Deserialize)]
pub struct Output {
    pub txid: Txid,
}

impl Send {
    pub(crate) fn run(self, wallet: &Wallet, network: Network) -> Result<Output> {
        wallet.sync(&CancellationToken::new())?;

        let address = self.address.require_network(network)?;
        let amount = Amount::from_sat(self.amount);
        let fee_rate = FeeRate::from_sat_per_vb(self.fee_rate).context("invalid fee rate")?;

        Ok(Output {
            txid: wallet.send(address, amount, fee_rate)?,
        })
    }
}
