use super::*;

#[derive(Debug, Clone)]
pub enum Event {
    Notify(Notify),
    SetDifficulty(Difficulty),
    Reconnect(ClientReconnect),
    Disconnected,
}
