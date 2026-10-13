use super::*;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("invalid payout address {address:?}: {source}"))]
    InvalidAddress { address: String, source: ParseError },
    #[snafu(display("payout address {address} is not valid on {network}"))]
    WrongNetwork { address: String, network: Network },
    #[snafu(display("funding script must be p2wpkh or p2tr"))]
    UnsupportedScript,
    #[snafu(display(
        "payouts sum to {} sat but funding is only {} sat",
        payouts.to_sat(),
        funding.to_sat()
    ))]
    ExceedsFunding { payouts: Amount, funding: Amount },
    #[snafu(display(
        "payout of {} sat to {address} is below dust limit of {} sat",
        amount.to_sat(),
        limit.to_sat()
    ))]
    BelowDust {
        address: Address,
        amount: Amount,
        limit: Amount,
    },
    #[snafu(display(
        "funding minus payouts of {} sat cannot cover fee of {} sat",
        available.to_sat(),
        fee.to_sat()
    ))]
    InsufficientForFee { available: Amount, fee: Amount },
    #[snafu(display("transaction weight of {weight} exceeds maximum of {max}"))]
    ExceedsWeight { weight: Weight, max: Weight },
    #[snafu(display("amount arithmetic overflow"))]
    Overflow,
    #[snafu(display("transaction would have no outputs"))]
    EmptyOutputs,
    #[snafu(display("failed to construct psbt: {source}"))]
    Psbt { source: bitcoin::psbt::Error },
}
