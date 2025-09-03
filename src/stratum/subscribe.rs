use super::*;

#[derive(Debug, PartialEq)]
pub struct Subscribe {
    pub user_agent: String,
    pub extranonce1: Option<String>,
}

impl Serialize for Subscribe {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let len = if self.extranonce1.is_some() { 2 } else { 1 };
        let mut seq = serializer.serialize_seq(Some(len))?;
        seq.serialize_element(&self.user_agent)?;
        if let Some(x1) = &self.extranonce1 {
            seq.serialize_element(x1)?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for Subscribe {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let (user_agent, extranonce1) = <(String, Option<String>)>::deserialize(deserializer)?;

        Ok(Subscribe {
            user_agent,
            extranonce1,
        })
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct SubscribeResult {
    pub subscriptions: Vec<(String, String)>,
    pub extranonce1: String,
    pub extranonce2_size: u32,
}

impl Serialize for SubscribeResult {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(3))?;
        seq.serialize_element(&self.subscriptions)?;
        seq.serialize_element(&self.extranonce1)?;
        seq.serialize_element(&self.extranonce2_size)?;
        seq.end()
    }
}

impl<'de> Deserialize<'de> for SubscribeResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let (subscriptions, extranonce1, extranonce2_size) =
            <(Vec<(String, String)>, String, u32)>::deserialize(deserializer)?;

        Ok(SubscribeResult {
            subscriptions,
            extranonce1,
            extranonce2_size,
        })
    }
}
