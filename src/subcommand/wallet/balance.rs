use super::*;

#[derive(Serialize, Deserialize)]
pub struct Output {
    pub confirmed: u64,
    pub pending: u64,
    pub total: u64,
}

pub(super) fn run(wallet: &Wallet) -> Result<Output> {
    wallet.sync()?;
    let balance = wallet.balance();

    Ok(Output {
        confirmed: balance.confirmed.to_sat(),
        pending: (balance.trusted_pending + balance.untrusted_pending).to_sat(),
        total: balance.total().to_sat(),
    })
}
