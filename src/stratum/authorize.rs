use super::*;

#[derive(Debug, PartialEq)]
pub struct Authorize {
    pub username: String,
    pub password: Option<String>,
}

impl Serialize for Authorize {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let len = if self.password.is_some() { 2 } else { 1 };
        let mut seq = serializer.serialize_seq(Some(len))?;
        seq.serialize_element(&self.username)?;
        if let Some(pass) = &self.password {
            seq.serialize_element(pass)?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for Authorize {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let (username, password) = <(String, Option<String>)>::deserialize(deserializer)?;
        Ok(Authorize { username, password })
    }
}
