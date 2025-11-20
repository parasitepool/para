use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, DeserializeFromStr, SerializeDisplay)]
pub struct Nonce(u32);

impl FromStr for Nonce {
    type Err = InternalError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let nonce = u32::from_str_radix(s, 16).map_err(|e| InternalError::Parse {
            message: format!("invalid nonce hex string '{}': {}", s, e),
        })?;
        Ok(Nonce(nonce))
    }
}

impl fmt::Display for Nonce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:08x}", self.0)
    }
}

impl From<Nonce> for u32 {
    fn from(n: Nonce) -> u32 {
        n.0
    }
}

impl From<u32> for Nonce {
    fn from(n: u32) -> Nonce {
        Nonce(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn some_nonces() {
        assert_eq!(Nonce::from(u32::MAX).to_string(), "ffffffff");
        assert_eq!(Nonce::from_str("ffffffff").unwrap(), Nonce::from(u32::MAX));

        assert_eq!(Nonce::from(u32::MAX - 1).to_string(), "fffffffe");
        assert_eq!(
            Nonce::from_str("fffffffe").unwrap(),
            Nonce::from(u32::MAX - 1)
        );

        assert_eq!(Nonce::from(0).to_string(), "00000000");
        assert_eq!(Nonce::from_str("00000000").unwrap(), Nonce::from(0));
    }
}
