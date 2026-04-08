use super::*;

#[derive(Serialize, Deserialize)]
pub struct Output {
    pub address: Address<NetworkUnchecked>,
}

pub(super) fn run(wallet: &Wallet) -> Output {
    Output {
        address: wallet.address().address.as_unchecked().clone(),
    }
}
