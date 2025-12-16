use super::*;
use bitcoin::address::NetworkUnchecked;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Username(pub String);

#[derive(Debug, Clone)]
pub struct ParsedUsername {
    pub address: Address,
    pub workername: String,
}

impl Username {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        self.0.trim_matches('"')
    }

    pub fn workername(&self) -> &str {
        self.as_str()
    }

    fn address_str(&self) -> Option<&str> {
        self.as_str().split('.').next()
    }

    pub fn parse_address(&self) -> std::result::Result<Address<NetworkUnchecked>, AddressError> {
        let address_str = self.address_str().ok_or(AddressError::EmptyUsername)?;
        Address::from_str(address_str).map_err(AddressError::Parse)
    }

    pub fn parse_with_network(&self, network: Network) -> Result<ParsedUsername, AddressError> {
        let address = self
            .parse_address()?
            .require_network(network)
            .map_err(|_| AddressError::NetworkMismatch {
                expected: network,
                address: self.parse_address().unwrap().assume_checked().to_string(),
            })?;
        Ok(ParsedUsername {
            address,
            workername: self.workername().to_string(),
        })
    }
}

impl std::fmt::Display for Username {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

#[derive(Debug)]
pub enum AddressError {
    EmptyUsername,
    Parse(bitcoin::address::ParseError),
    NetworkMismatch { expected: Network, address: String },
}

impl Display for AddressError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            AddressError::EmptyUsername => write!(f, "Username cannot be empty"),
            AddressError::Parse(e) => write!(f, "Invalid bitcoin address: {e}"),
            AddressError::NetworkMismatch { expected, address } => {
                write!(f, "Address {address} is not valid for {expected} network")
            }
        }
    }
}

impl std::error::Error for AddressError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn username_parse_address_only() {
        let username = Username::new("bc1qtest");
        assert_eq!(username.as_str(), "bc1qtest");
        assert_eq!(username.workername(), "bc1qtest");
    }

    #[test]
    fn username_parse_with_worker() {
        let username = Username::new("bc1qtest.worker1");
        assert_eq!(username.as_str(), "bc1qtest.worker1");
        assert_eq!(username.workername(), "bc1qtest.worker1");
    }

    #[test]
    fn username_strips_quotes() {
        let username = Username::new("\"bc1qtest.worker1\"");
        assert_eq!(username.as_str(), "bc1qtest.worker1");
    }

    #[test]
    fn username_serialize_roundtrip() {
        let username = Username::new("bc1qtest.worker1");
        let json = serde_json::to_string(&username).unwrap();
        assert_eq!(json, r#""bc1qtest.worker1""#);

        let parsed: Username = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, username);
    }
}
