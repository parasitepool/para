use super::*;

pub(crate) type RouterResult<T> = Result<T, RouterError>;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub(crate) enum RouterError {
    #[snafu(display("router is halted"))]
    Halted,
    #[snafu(display("hash days must be positive"))]
    InvalidHashdays,
    #[snafu(display("price calculation overflow"))]
    HashPriceOverflow,
    #[snafu(display("bid price {bid} is below minimum hash value {minimum}"))]
    HashPriceBelowMinimum { bid: HashPrice, minimum: HashValue },
    #[snafu(display("order total {amount} is below dust limit {dust_limit}"))]
    BelowDustLimit { amount: Amount, dust_limit: Amount },
    #[snafu(display("wallet is still syncing, try again shortly"))]
    WalletSyncing,
    #[snafu(display("wallet not configured, bucket orders unavailable"))]
    WalletRequired,
    #[snafu(display("wallet persistence failed: {error:#}"))]
    WalletPersistence { error: anyhow::Error },
    #[snafu(display("active order {id} is missing upstream"))]
    MissingActiveUpstream { id: u32 },
    #[snafu(display("active order {id} is missing extranonce allocator"))]
    MissingActiveAllocator { id: u32 },
}
