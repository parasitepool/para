use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Extranonce(Vec<u8>);

impl Extranonce {
    pub fn random(size: usize) -> Self {
        let mut v = vec![0u8; size];
        rand::rng().fill_bytes(&mut v);
        Self(v)
    }

    pub fn zeros(size: usize) -> Self {
        Self(vec![0u8; size])
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(bytes.to_vec())
    }

    pub fn increment_wrapping(&mut self) {
        for b in self.0.iter_mut().rev() {
            let (next, carry) = b.overflowing_add(1);
            *b = next;
            if !carry {
                return;
            }
        }
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        hex::encode(&self.0)
    }

    pub fn from_hex(s: &str) -> Result<Self, InternalError> {
        let bytes = hex::decode(s).context(error::HexParseSnafu)?;
        Ok(Self(bytes))
    }
}

impl Serialize for Extranonce {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&self.to_hex())
    }
}
impl<'de> Deserialize<'de> for Extranonce {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        Self::from_hex(&s).map_err(|e| serde::de::Error::custom(e.to_string()))
    }
}

impl fmt::Display for Extranonce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}
impl FromStr for Extranonce {
    type Err = InternalError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_hex(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_rejects_odd_length_hex() {
        assert!(
            "abc"
                .parse::<Extranonce>()
                .unwrap_err()
                .to_string()
                .contains("Odd number of digits")
        );
    }

    #[test]
    fn deserialize_rejects_non_hex() {
        assert!(
            "zz".parse::<Extranonce>()
                .unwrap_err()
                .to_string()
                .contains("Invalid character")
        );
    }

    #[test]
    fn valid_hex_roundtrip() {
        let enonce: Extranonce = serde_json::from_str(r#""abcd""#).unwrap();
        assert_eq!(enonce.len(), 2);
        assert_eq!(enonce.to_hex(), "abcd");
        let ser = serde_json::to_string(&enonce).unwrap();
        assert_eq!(ser, r#""abcd""#);
    }

    #[test]
    fn generate_has_correct_length() {
        let enonce2 = Extranonce::random(8);
        assert_eq!(enonce2.len(), 8);
        assert!(!enonce2.as_bytes().is_empty());
    }

    #[test]
    fn enonce_serializes_as_hex_string() {
        let enonce1: Extranonce = serde_json::from_str(r#""abcd""#).unwrap();
        let ser = serde_json::to_string(&enonce1).unwrap();
        assert_eq!(ser, r#""abcd""#);
    }

    #[test]
    fn increment() {
        let mut enonce = Extranonce::zeros(1);
        enonce.increment_wrapping();
        assert_eq!(enonce.to_hex(), "01");
    }

    #[test]
    fn increment_with_carry() {
        let mut enonce = "00ff".parse::<Extranonce>().unwrap();
        enonce.increment_wrapping();
        assert_eq!(enonce.to_hex(), "0100");
    }

    #[test]
    fn increment_multi_byte_carry() {
        let mut enonce = "00ffff".parse::<Extranonce>().unwrap();
        enonce.increment_wrapping();
        assert_eq!(enonce.to_hex(), "010000");
    }

    #[test]
    fn increment_wraps() {
        let mut enonce = "ffff".parse::<Extranonce>().unwrap();
        enonce.increment_wrapping();
        assert_eq!(enonce.to_hex(), "0000");
        assert_eq!(enonce.len(), 2);
    }

    #[test]
    fn increment_sequence() {
        let mut enonce = "00fe".parse::<Extranonce>().unwrap();
        enonce.increment_wrapping();
        assert_eq!(enonce.to_hex(), "00ff");
        enonce.increment_wrapping();
        assert_eq!(enonce.to_hex(), "0100");
    }

    #[test]
    fn from_bytes_roundtrip() {
        let enonce = Extranonce::from_bytes(&[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(enonce.len(), 4);
        assert_eq!(enonce.to_hex(), "deadbeef");
        assert_eq!(enonce.as_bytes(), &[0xde, 0xad, 0xbe, 0xef]);
    }
}
