use super::*;

#[derive(Debug, Clone)]
pub enum StratumEvent {
    Notify(Notify),
    SetDifficulty(Difficulty),
    Disconnected,
}
