use super::*;

/// Worker identity with Bitcoin address parsing
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Username {
    raw: String,
    address: Address<NetworkUnchecked>,
    workername: String,
}

impl Username {
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    pub fn address(&self) -> &Address<NetworkUnchecked> {
        &self.address
    }

    pub fn workername(&self) -> &str {
        &self.workername
    }

    pub fn parse_with_network(&self, network: Network) -> Result<Address, InternalError> {
        self.address
            .clone()
            .require_network(network)
            .map_err(|_| InternalError::NetworkMismatch {
                expected: network,
                address: self.address.clone().assume_checked().to_string(),
            })
    }

    pub fn infer_network(&self) -> Result<Network, InternalError> {
        const NETWORKS: &[Network] = &[
            Network::Bitcoin,
            Network::Testnet4,
            Network::Testnet,
            Network::Signet,
            Network::Regtest,
        ];

        for &network in NETWORKS {
            if self.address.clone().require_network(network).is_ok() {
                return Ok(network);
            }
        }

        Err(InternalError::UnknownNetwork)
    }
}

impl Display for Username {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.raw)
    }
}

impl FromStr for Username {
    type Err = InternalError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let raw = s.to_string();

        if raw.is_empty() {
            return Err(InternalError::EmptyUsername);
        }

        let (address, workername) = raw.split_once('.').ok_or_else(|| InternalError::Parse {
            message: "username must include workername".into(),
        })?;
        let workername = workername.to_string();

        if workername.is_empty() {
            return Err(InternalError::Parse {
                message: "username must include workername".into(),
            });
        }

        let address = Address::from_str(address)
            .map_err(|source| InternalError::InvalidAddress { source })?;

        Ok(Self {
            raw,
            address,
            workername,
        })
    }
}

impl TryFrom<String> for Username {
    type Error = InternalError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Username> for String {
    fn from(value: Username) -> Self {
        value.raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn username_parse_with_worker() {
        let username: Username = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.worker1"
            .parse()
            .unwrap();

        assert!(
            username.parse_with_network(Network::Bitcoin).is_ok(),
            "address is a valid mainnet address"
        );
        assert_eq!(
            username.as_str(),
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.worker1"
        );
        assert_eq!(username.workername(), "worker1");
        assert_eq!(
            username.address().clone().assume_checked().to_string(),
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4"
        );
    }

    #[test]
    fn username_parse_with_legacy_address() {
        let username: Username = "3EktnHQD7RiAE6uzMj2ZifT9YgRrkSgzQX.worker1"
            .parse()
            .unwrap();

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
    fn username_serialize_roundtrip() {
        let username: Username = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.worker1"
            .parse()
            .unwrap();
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
        assert!(
            "testvaluenotanaddress.workername.extrafluff"
                .parse::<Username>()
                .is_err(),
            "address is rejected due to being invalid"
        );
    }

    #[test]
    fn username_rejects_invalid_network() {
        let username: Username = "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx.worker1"
            .parse()
            .unwrap();

        assert!(
            username.parse_with_network(Network::Bitcoin).is_err(),
            "address is rejected due to being signet and requesting mainnet"
        );
    }

    #[test]
    fn infer_network_mainnet_bech32() {
        let username: Username = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.worker1"
            .parse()
            .unwrap();
        assert_eq!(username.infer_network().unwrap(), Network::Bitcoin);
    }

    #[test]
    fn infer_network_mainnet_p2sh() {
        let username: Username = "3EktnHQD7RiAE6uzMj2ZifT9YgRrkSgzQX.worker1"
            .parse()
            .unwrap();
        assert_eq!(username.infer_network().unwrap(), Network::Bitcoin);
    }

    #[test]
    fn infer_network_mainnet_p2pkh() {
        let username: Username = "1CPDJtMzuSyvnGi8o9ZAtAWPfqHZhjQQhB.worker1"
            .parse()
            .unwrap();
        assert_eq!(username.infer_network().unwrap(), Network::Bitcoin);
    }

    #[test]
    fn infer_network_testnet() {
        let username: Username = "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx.worker1"
            .parse()
            .unwrap();
        // tb1 prefix is valid for both Testnet and Testnet4; Testnet4 is checked first
        assert_eq!(username.infer_network().unwrap(), Network::Testnet4);
    }

    #[test]
    fn infer_network_with_worker() {
        let username: Username = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.worker1"
            .parse()
            .unwrap();
        assert_eq!(username.infer_network().unwrap(), Network::Bitcoin);
    }

    #[test]
    fn infer_network_invalid_address() {
        assert!("notanaddress.worker1".parse::<Username>().is_err());
    }

    #[test]
    fn username_requires_workername() {
        assert!(
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4"
                .parse::<Username>()
                .is_err()
        );
        assert!(
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4."
                .parse::<Username>()
                .is_err()
        );
    }

    #[test]
    fn address() {
        #[track_caller]
        fn case(input: &str, expected: &str) {
            assert_eq!(
                input
                    .parse::<Username>()
                    .unwrap()
                    .address()
                    .clone()
                    .assume_checked()
                    .to_string(),
                expected
            );
        }

        case(
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.worker1",
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4",
        );
        case(
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.worker1",
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4",
        );
        case(
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.worker.foo",
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4",
        );
    }
}
