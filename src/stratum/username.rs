use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Username(pub String);

impl Username {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        self.0.trim_matches('"')
    }

    pub fn workername(&self) -> &str {
        self.as_str().split('.').nth(1).unwrap_or("")
    }

    fn address_str(&self) -> Option<&str> {
        self.as_str().split('.').next()
    }

    pub fn parse_address(&self) -> std::result::Result<Address<NetworkUnchecked>, InternalError> {
        let address_str = self.address_str().ok_or(InternalError::EmptyUsername)?;
        Address::from_str(address_str).map_err(|source| InternalError::InvalidAddress { source })
    }

    pub fn parse_with_network(&self, network: Network) -> Result<Address, InternalError> {
        self.parse_address()?
            .require_network(network)
            .map_err(|_| InternalError::NetworkMismatch {
                expected: network,
                address: self.parse_address().unwrap().assume_checked().to_string(),
            })
    }

    pub fn infer_network(&self) -> Result<Network, InternalError> {
        let unchecked = self.parse_address()?;

        const NETWORKS: &[Network] = &[
            Network::Bitcoin,
            Network::Testnet4,
            Network::Testnet,
            Network::Signet,
            Network::Regtest,
        ];

        for &network in NETWORKS {
            if unchecked.clone().require_network(network).is_ok() {
                return Ok(network);
            }
        }

        Err(InternalError::UnknownNetwork)
    }
}

impl Display for Username {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for Username {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for Username {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn username_parse_address_only() {
        let username = Username::new("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4");

        assert!(
            username.parse_with_network(Network::Bitcoin).is_ok(),
            "address is a valid mainnet address"
        );
        assert_eq!(
            username.as_str(),
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4"
        );
        assert_eq!(username.workername(), "");
    }

    #[test]
    fn username_parse_with_worker() {
        let username = Username::new("3EktnHQD7RiAE6uzMj2ZifT9YgRrkSgzQX.worker1");

        assert!(
            username.parse_with_network(Network::Bitcoin).is_ok(),
            "address is a valid mainnet address"
        );
        assert_eq!(
            username.as_str(),
            "3EktnHQD7RiAE6uzMj2ZifT9YgRrkSgzQX.worker1"
        );
        assert_eq!(username.workername(), "worker1");
    }

    #[test]
    fn username_strips_quotes() {
        let username = Username::new("\"1CPDJtMzuSyvnGi8o9ZAtAWPfqHZhjQQhB.worker1\"");
        assert!(
            username.parse_with_network(Network::Bitcoin).is_ok(),
            "address is a valid mainnet address"
        );
        assert_eq!(
            username.as_str(),
            "1CPDJtMzuSyvnGi8o9ZAtAWPfqHZhjQQhB.worker1"
        );
    }

    #[test]
    fn username_serialize_roundtrip() {
        let username = Username::new("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.worker1");
        let json = serde_json::to_string(&username).unwrap();
        assert_eq!(
            json,
            r#""bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.worker1""#
        );

        let parsed: Username = serde_json::from_str(&json).unwrap();
        assert!(
            parsed.parse_with_network(Network::Bitcoin).is_ok(),
            "address is a valid mainnet address"
        );
        assert_eq!(parsed, username);
    }

    #[test]
    fn username_rejects_invalid_address() {
        let username = Username::new("testvaluenotanaddress.workername.extrafluff");

        assert!(
            username.parse_address().is_err(),
            "address is rejected due to being invalid"
        );
    }

    #[test]
    fn username_rejects_invalid_network() {
        let username = Username::new("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4");

        assert!(
            username.parse_with_network(Network::Bitcoin).is_err(),
            "address is rejected due to being signet and requesting mainnet"
        );
    }

    #[test]
    fn infer_network_mainnet_bech32() {
        let username = Username::new("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4");
        assert_eq!(username.infer_network().unwrap(), Network::Bitcoin);
    }

    #[test]
    fn infer_network_mainnet_p2sh() {
        let username = Username::new("3EktnHQD7RiAE6uzMj2ZifT9YgRrkSgzQX");
        assert_eq!(username.infer_network().unwrap(), Network::Bitcoin);
    }

    #[test]
    fn infer_network_mainnet_p2pkh() {
        let username = Username::new("1CPDJtMzuSyvnGi8o9ZAtAWPfqHZhjQQhB");
        assert_eq!(username.infer_network().unwrap(), Network::Bitcoin);
    }

    #[test]
    fn infer_network_testnet() {
        let username = Username::new("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx");
        // tb1 prefix is valid for both Testnet and Testnet4; Testnet4 is checked first
        assert_eq!(username.infer_network().unwrap(), Network::Testnet4);
    }

    #[test]
    fn infer_network_with_worker() {
        let username = Username::new("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.worker1");
        assert_eq!(username.infer_network().unwrap(), Network::Bitcoin);
    }

    #[test]
    fn infer_network_invalid_address() {
        let username = Username::new("notanaddress");
        assert!(username.infer_network().is_err());
    }
}
