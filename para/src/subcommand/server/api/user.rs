use super::*;

#[derive(Debug, Deserialize, Serialize, PartialEq)]
pub(crate) struct User {
    pub(crate) hashrate1m: String, // could be HashRate if adapted
    pub(crate) hashrate5m: String,
    pub(crate) hashrate1hr: String,
    pub(crate) hashrate1d: String,
    pub(crate) hashrate7d: String,
    pub(crate) lastshare: u64, // I think this is a unix time
    pub(crate) workers: u64,
    pub(crate) shares: u64,
    pub(crate) bestshare: f64,
    pub(crate) bestever: u64,
    pub(crate) authorised: u64, // This is unix time
    pub(crate) worker: Vec<Worker>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
pub(crate) struct Worker {
    pub(crate) workername: String, // make HashRate type
    pub(crate) hashrate1m: String,
    pub(crate) hashrate5m: String,
    pub(crate) hashrate1hr: String,
    pub(crate) hashrate1d: String,
    pub(crate) hashrate7d: String,
    pub(crate) lastshare: u64, //unix time
    pub(crate) shares: u64,
    pub(crate) bestshare: f64,
    pub(crate) bestever: u64,
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
}
