use super::*;

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub(crate) struct User {
    pub(crate) hashrate1m: HashRate,
    pub(crate) hashrate5m: HashRate,
    pub(crate) hashrate1hr: HashRate,
    pub(crate) hashrate1d: HashRate,
    pub(crate) hashrate7d: HashRate,
    pub(crate) lastshare: u64,
    pub(crate) workers: u64,
    pub(crate) shares: u64,
    pub(crate) bestshare: f64,
    pub(crate) bestever: u64,
    pub(crate) authorised: u64,
    pub(crate) worker: Vec<Worker>,
}

impl Add for User {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        let worker = self
            .worker
            .into_iter()
            .chain(rhs.worker)
            .fold(HashMap::new(), |mut acc, w| {
                acc.entry(w.workername.clone())
                    .and_modify(|existing: &mut Worker| *existing = existing.clone() + w.clone())
                    .or_insert(w);
                acc
            })
            .into_values()
            .collect();

        Self {
            hashrate1m: self.hashrate1m + rhs.hashrate1m,
            hashrate5m: self.hashrate5m + rhs.hashrate5m,
            hashrate1hr: self.hashrate1hr + rhs.hashrate1hr,
            hashrate1d: self.hashrate1d + rhs.hashrate1d,
            hashrate7d: self.hashrate7d + rhs.hashrate7d,
            lastshare: self.lastshare.max(rhs.lastshare),
            workers: self.workers + rhs.workers,
            shares: self.shares + rhs.shares,
            bestshare: self.bestshare.max(rhs.bestshare),
            bestever: self.bestever.max(rhs.bestever),
            authorised: self.authorised.min(rhs.authorised),
            worker,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub(crate) struct Worker {
    pub(crate) workername: String,
    pub(crate) hashrate1m: HashRate,
    pub(crate) hashrate5m: HashRate,
    pub(crate) hashrate1hr: HashRate,
    pub(crate) hashrate1d: HashRate,
    pub(crate) hashrate7d: HashRate,
    pub(crate) lastshare: u64,
    pub(crate) shares: u64,
    pub(crate) bestshare: f64,
    pub(crate) bestever: u64,
}

impl Add for Worker {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        assert_eq!(self.workername, rhs.workername);

        Self {
            workername: self.workername,
            hashrate1m: self.hashrate1m + rhs.hashrate1m,
            hashrate5m: self.hashrate5m + rhs.hashrate5m,
            hashrate1hr: self.hashrate1hr + rhs.hashrate1hr,
            hashrate1d: self.hashrate1d + rhs.hashrate1d,
            hashrate7d: self.hashrate7d + rhs.hashrate7d,
            lastshare: self.lastshare.max(rhs.lastshare),
            shares: self.shares + rhs.shares,
            bestshare: self.bestshare.max(rhs.bestshare),
            bestever: self.bestever.max(rhs.bestever),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER_1: &str = r#"
{
 "hashrate1m": "0",
 "hashrate5m": "0",
 "hashrate1hr": "4.57G",
 "hashrate1d": "85.4T",
 "hashrate7d": "148T",
 "lastshare": 1751962081,
 "workers": 0,
 "shares": 7831260707329,
 "bestshare": 137427866914851.2,
 "bestever": 137427866914851,
 "authorised": 1721981103,
 "worker": [
  {
   "workername": "bc1qa8r4up9nchkvdnhcf9feexv2jfantrk48ef374",
   "hashrate1m": "0",
   "hashrate5m": "0",
   "hashrate1hr": "4.57G",
   "hashrate1d": "85.4T",
   "hashrate7d": "51.5T",
   "lastshare": 1751962081,
   "shares": 10267078458,
   "bestshare": 203034806650.409,
   "bestever": 203034806650
  }
 ]
}"#;

    const USER_2: &str = r#"
{
 "hashrate1m": "0",
 "hashrate5m": "0",
 "hashrate1hr": "0",
 "hashrate1d": "0",
 "hashrate7d": "13.6G",
 "lastshare": 1749654735,
 "workers": 0,
 "shares": 175543390715,
 "bestshare": 9775540.926479112,
 "bestever": 52629282507,
 "authorised": 1741143401,
 "worker": []
}"#;

    const USER_3: &str = r#"
{
 "hashrate1m": "0",
 "hashrate5m": "0",
 "hashrate1hr": "4.28G",
 "hashrate1d": "85.2T",
 "hashrate7d": "148T",
 "lastshare": 1751962081,
 "workers": 0,
 "shares": 7831260707329,
 "bestshare": 137427866914851.2,
 "bestever": 137427866914851,
 "authorised": 1721981103,
 "worker": [
  {
   "workername": "bc1qa8r4up9nchkvdnhcf9feexv2jfantrk48ef374",
   "hashrate1m": "0",
   "hashrate5m": "0",
   "hashrate1hr": "4.28G",
   "hashrate1d": "85.2T",
   "hashrate7d": "51.4T",
   "lastshare": 1751962081,
   "shares": 10267078458,
   "bestshare": 203034806650.409,
   "bestever": 203034806650
  }
 ]
}"#;

    #[test]
    fn user_from_json() {
        assert!(serde_json::from_str::<User>(USER_1).is_ok());
        assert!(serde_json::from_str::<User>(USER_2).is_ok());
        assert!(serde_json::from_str::<User>(USER_3).is_ok());
    }

    #[test]
    fn test_worker_addition() {
        let worker1 = Worker {
            workername: "test_worker".to_string(),
            hashrate1m: HashRate(1e3),
            hashrate5m: HashRate(2e6),
            hashrate1hr: HashRate(3e9),
            hashrate1d: HashRate(4e12),
            hashrate7d: HashRate(5e15),
            lastshare: 1000,
            shares: 500,
            bestshare: 1000.0,
            bestever: 2000,
        };

        let worker2 = Worker {
            workername: "test_worker".to_string(),
            hashrate1m: HashRate(100.0),
            hashrate5m: HashRate(200.0),
            hashrate1hr: HashRate(300.0),
            hashrate1d: HashRate(400.0),
            hashrate7d: HashRate(500.0),
            lastshare: 1200,
            shares: 600,
            bestshare: 1500.0,
            bestever: 2500,
        };

        let sum = worker1 + worker2;

        assert_eq!(sum.workername, "test_worker");
        assert_eq!(sum.hashrate1m.0, 1e3 + 100.0);
        assert_eq!(sum.hashrate5m.0, 2e6 + 200.0);
        assert_eq!(sum.hashrate1hr.0, 3e9 + 300.0);
        assert_eq!(sum.hashrate1d.0, 4e12 + 400.0);
        assert_eq!(sum.hashrate7d.0, 5e15 + 500.0);
        assert_eq!(sum.lastshare, 1200);
        assert_eq!(sum.shares, 1100);
        assert_eq!(sum.bestshare, 1500.0);
        assert_eq!(sum.bestever, 2500);
    }

    #[test]
    fn test_user_addition() {
        let user1: User = serde_json::from_str(USER_1).unwrap();
        let user3: User = serde_json::from_str(USER_3).unwrap();

        let sum = user1.clone() + user3.clone();

        assert_eq!(sum.hashrate1m.0, 0.0 + 0.0);
        assert_eq!(sum.hashrate5m.0, 0.0 + 0.0);
        assert_eq!(sum.hashrate1hr.0, 4.57e9 + 4.28e9);
        assert_eq!(sum.hashrate1d.0, 85.4e12 + 85.2e12);
        assert_eq!(sum.hashrate7d.0, 148e12 + 148e12);
        assert_eq!(sum.lastshare, 1751962081);
        assert_eq!(sum.workers, 0);
        assert_eq!(sum.shares, 7831260707329 + 7831260707329);
        assert_eq!(sum.bestshare, 137427866914851.2);
        assert_eq!(sum.bestever, 137427866914851);
        assert_eq!(sum.authorised, 1721981103);
        assert_eq!(sum.worker.len(), 1);
        let merged_worker = &sum.worker[0];
        assert_eq!(merged_worker.hashrate1hr.0, 4.57e9 + 4.28e9);
        assert_eq!(merged_worker.hashrate1d.0, 85.4e12 + 85.2e12);
        assert_eq!(merged_worker.hashrate7d.0, 51.5e12 + 51.4e12);
        assert_eq!(merged_worker.shares, 10267078458 * 2);
        assert_eq!(merged_worker.bestshare, 203034806650.409);
        assert_eq!(merged_worker.bestever, 203034806650);

        let user2: User = serde_json::from_str(USER_2).unwrap();
        let sum_with_empty = user1 + user2;
        assert_eq!(sum_with_empty.worker.len(), 1);
        assert_eq!(sum_with_empty.workers, 0);
        assert_eq!(sum_with_empty.authorised, 1721981103);
    }
}
