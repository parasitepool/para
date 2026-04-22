use super::*;

pub(crate) type RouterResult<T> = Result<T, RouterError>;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub(crate) enum RouterError {
    #[snafu(display("hash days must be positive"))]
    InvalidHashdays,
    #[snafu(display("price calculation overflow"))]
    HashPriceOverflow,
    #[snafu(display("bid price {bid} is below minimum {minimum}"))]
    HashPriceBelowMinimum { bid: HashPrice, minimum: HashPrice },
    #[snafu(display("wallet is still syncing, try again shortly"))]
    WalletSyncing,
}
