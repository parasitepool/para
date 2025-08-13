use super::*;

pub use user::{User, Worker};

mod user;

#[derive(Debug, DeserializeFromStr, SerializeDisplay, PartialEq, Clone, Copy)]
pub struct Status {
    pub pool: PoolStatus,
    pub hash_rates: HashRateStatus,
    pub shares: ShareStatus,
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

impl Add for Status {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self {
            pool: self.pool + rhs.pool,
            hash_rates: self.hash_rates + rhs.hash_rates,
            shares: self.shares + rhs.shares,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Copy, Clone)]
pub struct PoolStatus {
    pub runtime: u64,
    pub lastupdate: u64,
    #[serde(rename = "Users")]
    pub users: u64,
    #[serde(rename = "Workers")]
    pub workers: u64,
    #[serde(rename = "Idle")]
    pub idle: u64,
    #[serde(rename = "Disconnected")]
    pub disconnected: u64,
}

impl Add for PoolStatus {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self {
            runtime: self.runtime.max(rhs.runtime),
            lastupdate: self.lastupdate.max(rhs.lastupdate),
            users: self.users + rhs.users,
            workers: self.workers + rhs.workers,
            idle: self.idle + rhs.idle,
            disconnected: self.disconnected + rhs.disconnected,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone, Copy)]
pub struct HashRateStatus {
    pub hashrate1m: HashRate,
    pub hashrate5m: HashRate,
    pub hashrate15m: HashRate,
    pub hashrate1hr: HashRate,
    pub hashrate6hr: HashRate,
    pub hashrate1d: HashRate,
    pub hashrate7d: HashRate,
}

impl Add for HashRateStatus {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self {
            hashrate1m: self.hashrate1m + rhs.hashrate1m,
            hashrate5m: self.hashrate5m + rhs.hashrate5m,
            hashrate15m: self.hashrate15m + rhs.hashrate15m,
            hashrate1hr: self.hashrate1hr + rhs.hashrate1hr,
            hashrate6hr: self.hashrate6hr + rhs.hashrate6hr,
            hashrate1d: self.hashrate1d + rhs.hashrate1d,
            hashrate7d: self.hashrate7d + rhs.hashrate7d,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone, Copy)]
pub struct ShareStatus {
    pub diff: f64,
    pub accepted: u64,
    pub rejected: u64,
    pub bestshare: u64,
    #[serde(rename = "SPS1m")]
    pub sps1m: f64,
    #[serde(rename = "SPS5m")]
    pub sps5m: f64,
    #[serde(rename = "SPS15m")]
    pub sps15m: f64,
    #[serde(rename = "SPS1h")]
    pub sps1h: f64,
}

impl Add for ShareStatus {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self {
            diff: self.diff + rhs.diff,
            accepted: self.accepted + rhs.accepted,
            rejected: self.rejected + rhs.rejected,
            bestshare: self.bestshare.max(rhs.bestshare),
            sps1m: self.sps1m + rhs.sps1m,
            sps5m: self.sps5m + rhs.sps5m,
            sps15m: self.sps15m + rhs.sps15m,
            sps1h: self.sps1h + rhs.sps1h,
        }
    }
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
        assert_eq!(
            status.hash_rates.hashrate1m,
            HashRate::from_str("314P").unwrap()
        );
        assert_eq!(
            status.hash_rates.hashrate1d,
            HashRate::from_str("274P").unwrap()
        );
        assert_eq!(status.shares.diff, 76.2);
        assert_eq!(status.shares.accepted, 89150201900099);
        assert_eq!(status.shares.sps1h, 3920.0);
    }

    #[test]
    fn test_pool_status_addition() {
        let a = PoolStatus {
            runtime: 100,
            lastupdate: 200,
            users: 10,
            workers: 20,
            idle: 5,
            disconnected: 2,
        };
        let b = PoolStatus {
            runtime: 150,
            lastupdate: 180,
            users: 15,
            workers: 25,
            idle: 3,
            disconnected: 4,
        };
        let sum = a + b;
        assert_eq!(sum.runtime, 150);
        assert_eq!(sum.lastupdate, 200);
        assert_eq!(sum.users, 25);
        assert_eq!(sum.workers, 45);
        assert_eq!(sum.idle, 8);
        assert_eq!(sum.disconnected, 6);
    }

    #[test]
    fn test_hashrate_status_addition() {
        let status1 = HashRateStatus {
            hashrate1m: HashRate(1e3),
            hashrate5m: HashRate(2e6),
            hashrate15m: HashRate(3e9),
            hashrate1hr: HashRate(4e12),
            hashrate6hr: HashRate(5e15),
            hashrate1d: HashRate(6e18),
            hashrate7d: HashRate(7e21),
        };

        let status2 = HashRateStatus {
            hashrate1m: HashRate(100.0),
            hashrate5m: HashRate(200.5),
            hashrate15m: HashRate(300.0),
            hashrate1hr: HashRate(400.0),
            hashrate6hr: HashRate(500.0),
            hashrate1d: HashRate(600.0),
            hashrate7d: HashRate(700.0),
        };

        let sum = status1 + status2;

        let expected = HashRateStatus {
            hashrate1m: HashRate(1e3 + 100.0),
            hashrate5m: HashRate(2e6 + 200.5),
            hashrate15m: HashRate(3e9 + 300.0),
            hashrate1hr: HashRate(4e12 + 400.0),
            hashrate6hr: HashRate(5e15 + 500.0),
            hashrate1d: HashRate(6e18 + 600.0),
            hashrate7d: HashRate(7e21 + 700.0),
        };

        assert_eq!(sum, expected);

        let status3 = HashRateStatus {
            hashrate1m: HashRate::from_str("314P").unwrap(),
            hashrate5m: HashRate::from_str("1.23E").unwrap(),
            ..status1
        };

        let status4 = HashRateStatus {
            hashrate1m: HashRate::from_str("2T").unwrap(),
            hashrate5m: HashRate::from_str("3G").unwrap(),
            ..status2
        };

        let sum_parsed = status3 + status4;

        assert_eq!(sum_parsed.hashrate1m.0, 314e15 + 2e12);
        assert_eq!(sum_parsed.hashrate5m.0, 1.23e18 + 3e9);
    }

    #[test]
    fn test_share_status_addition() {
        let a = ShareStatus {
            diff: 50.0,
            accepted: 1000,
            rejected: 10,
            bestshare: 500,
            sps1m: 100.0,
            sps5m: 200.0,
            sps15m: 300.0,
            sps1h: 400.0,
        };
        let b = ShareStatus {
            diff: 30.0,
            accepted: 2000,
            rejected: 20,
            bestshare: 600,
            sps1m: 150.0,
            sps5m: 250.0,
            sps15m: 350.0,
            sps1h: 450.0,
        };
        let sum = a + b;
        assert_eq!(sum.diff, 80.0);
        assert_eq!(sum.accepted, 3000);
        assert_eq!(sum.rejected, 30);
        assert_eq!(sum.bestshare, 600);
        assert_eq!(sum.sps1m, 250.0);
        assert_eq!(sum.sps5m, 450.0);
        assert_eq!(sum.sps15m, 650.0);
        assert_eq!(sum.sps1h, 850.0);
    }

    #[test]
    fn test_status_addition() {
        let status1 = Status {
            pool: PoolStatus {
                runtime: 100,
                lastupdate: 200,
                users: 10,
                workers: 20,
                idle: 5,
                disconnected: 2,
            },
            hash_rates: HashRateStatus {
                hashrate1m: HashRate(1e3),
                hashrate5m: HashRate(2e6),
                hashrate15m: HashRate(3e9),
                hashrate1hr: HashRate(4e12),
                hashrate6hr: HashRate(5e15),
                hashrate1d: HashRate(6e18),
                hashrate7d: HashRate(7e21),
            },
            shares: ShareStatus {
                diff: 50.0,
                accepted: 1000,
                rejected: 10,
                bestshare: 500,
                sps1m: 100.0,
                sps5m: 200.0,
                sps15m: 300.0,
                sps1h: 400.0,
            },
        };

        let status2 = Status {
            pool: PoolStatus {
                runtime: 150,
                lastupdate: 180,
                users: 15,
                workers: 25,
                idle: 3,
                disconnected: 4,
            },
            hash_rates: HashRateStatus {
                hashrate1m: HashRate(100.0),
                hashrate5m: HashRate(200.5),
                hashrate15m: HashRate(300.0),
                hashrate1hr: HashRate(400.0),
                hashrate6hr: HashRate(500.0),
                hashrate1d: HashRate(600.0),
                hashrate7d: HashRate(700.0),
            },
            shares: ShareStatus {
                diff: 30.0,
                accepted: 2000,
                rejected: 20,
                bestshare: 600,
                sps1m: 150.0,
                sps5m: 250.0,
                sps15m: 350.0,
                sps1h: 450.0,
            },
        };

        let sum = status1 + status2;

        assert_eq!(sum.pool.runtime, 150);
        assert_eq!(sum.pool.lastupdate, 200);
        assert_eq!(sum.pool.users, 25);
        assert_eq!(sum.pool.workers, 45);
        assert_eq!(sum.pool.idle, 8);
        assert_eq!(sum.pool.disconnected, 6);

        assert_eq!(sum.hash_rates.hashrate1m.0, 1e3 + 100.0);
        assert_eq!(sum.hash_rates.hashrate5m.0, 2e6 + 200.5);
        assert_eq!(sum.hash_rates.hashrate15m.0, 3e9 + 300.0);
        assert_eq!(sum.hash_rates.hashrate1hr.0, 4e12 + 400.0);
        assert_eq!(sum.hash_rates.hashrate6hr.0, 5e15 + 500.0);
        assert_eq!(sum.hash_rates.hashrate1d.0, 6e18 + 600.0);
        assert_eq!(sum.hash_rates.hashrate7d.0, 7e21 + 700.0);

        assert_eq!(sum.shares.diff, 80.0);
        assert_eq!(sum.shares.accepted, 3000);
        assert_eq!(sum.shares.rejected, 30);
        assert_eq!(sum.shares.bestshare, 600);
        assert_eq!(sum.shares.sps1m, 250.0);
        assert_eq!(sum.shares.sps5m, 450.0);
        assert_eq!(sum.shares.sps15m, 650.0);
        assert_eq!(sum.shares.sps1h, 850.0);

        let parsed_status = Status::from_str(POOL_STATUS).unwrap();
        let sum_with_parsed = status1 + parsed_status;
        assert_eq!(
            sum_with_parsed.pool.runtime,
            parsed_status.pool.runtime.max(100)
        );
        assert_eq!(sum_with_parsed.hash_rates.hashrate1m.0, 1e3 + 314e15);
        assert_eq!(
            sum_with_parsed.shares.bestshare,
            parsed_status.shares.bestshare.max(500)
        );
    }
}
