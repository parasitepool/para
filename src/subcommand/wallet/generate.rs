use super::*;

#[derive(Serialize, Deserialize)]
pub struct Output {
    pub mnemonic: String,
    pub descriptor: String,
    pub change_descriptor: String,
}

pub(super) fn run(network: Network) -> Result<Output> {
    let (mnemonic, descriptor, change_descriptor) = Wallet::generate(network)?;

    Ok(Output {
        mnemonic,
        descriptor,
        change_descriptor,
    })
}
