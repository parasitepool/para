use super::*;

#[derive(Debug, DeserializeFromStr, SerializeDisplay, PartialEq)]
pub(crate) struct Status {
    pub(crate) pool: PoolStatus,
    pub(crate) hash_rates: HashRateStatus,
    pub(crate) shares: ShareStatus,
}

impl FromStr for Status {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut lines = s.lines();
        let pool =
            serde_json::from_str(lines.next().ok_or_else(|| anyhow!("Missing PoolStatus"))?)?;

        let hash_rates = serde_json::from_str(
            lines
                .next()
                .ok_or_else(|| anyhow!("Missing HashRateStatus"))?,
        )?;

        let shares =
            serde_json::from_str(lines.next().ok_or_else(|| anyhow!("Missing ShareStatus"))?)?;

        Ok(Status {
            pool,
            hash_rates,
            shares,
        })
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "{}",
            serde_json::to_string(&self.pool).map_err(|_| fmt::Error)?
        )?;
        writeln!(
            f,
            "{}",
            serde_json::to_string(&self.hash_rates).map_err(|_| fmt::Error)?
        )?;
        writeln!(
            f,
            "{}",
            serde_json::to_string(&self.shares).map_err(|_| fmt::Error)?
        )?;
        Ok(())
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
pub(crate) struct PoolStatus {
    runtime: u64,
    lastupdate: u64,
    #[serde(rename = "Users")]
    users: u64,
    #[serde(rename = "Workers")]
    workers: u64,
    #[serde(rename = "Idle")]
    idle: u64,
    #[serde(rename = "Disconnected")]
    disconnected: u64,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
pub(crate) struct HashRateStatus {
    hashrate1m: String,
    hashrate5m: String,
    hashrate15m: String,
    hashrate1hr: String,
    hashrate6hr: String,
    hashrate1d: String,
    hashrate7d: String,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
pub(crate) struct ShareStatus {
    diff: f64, // no idea what this is but some sort of percentage; 100% means work for one block achieved
    accepted: u64,
    rejected: u64,
    bestshare: u64, // maybe a f64, see above
    #[serde(rename = "SPS1m")]
    sps1m: f64,
    #[serde(rename = "SPS5m")]
    sps5m: f64,
    #[serde(rename = "SPS15m")]
    sps15m: f64,
    #[serde(rename = "SPS1h")]
    sps1h: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    const POOL_STATUS: &str = r#"{"runtime":2373426,"lastupdate":1752001916,"Users":12729,"Workers":50345,"Idle":8966,"Disconnected":2213}
{"hashrate1m":"314P","hashrate5m":"322P","hashrate15m":"311P","hashrate1hr":"360P","hashrate6hr":"316P","hashrate1d":"274P","hashrate7d":"183P"}
{"diff":76.2,"accepted":89150201900099,"rejected":788358901413,"bestshare":83821924668426,"SPS1m":3.92e3,"SPS5m":3.91e3,"SPS15m":3.91e3,"SPS1h":3.92e3}
"#;

    #[test]
    fn status_from_string() {
        let status = Status::from_str(POOL_STATUS).unwrap();
        assert_eq!(status.pool.runtime, 2373426);
        assert_eq!(status.pool.disconnected, 2213);
        assert_eq!(status.hash_rates.hashrate1m, "314P".to_string());
        assert_eq!(status.hash_rates.hashrate1d, "274P".to_string());
        assert_eq!(status.shares.diff, 76.2);
        assert_eq!(status.shares.accepted, 89150201900099);
        assert_eq!(status.shares.sps1h, 3920.0);
    }
}
