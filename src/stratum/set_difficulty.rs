use super::*;

#[derive(Debug, Serialize, Deserialize)]
pub struct SetDifficulty(pub Vec<Difficulty>);

impl SetDifficulty {
    pub fn difficulty(&self) -> Difficulty {
        *self.0.first().unwrap()
    }
}
