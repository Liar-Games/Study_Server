use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type SessionId = Uuid;
pub type RoomId = u64;
pub const GLOBAL_ROOM: RoomId = 0;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Principal {
    Account(Uuid),
    Guest(Uuid),
}

impl Principal {
    pub fn guest() -> Self {
        Self::Guest(Uuid::new_v4())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Player {
    pub session_id: SessionId,
    pub principal: Principal,
    pub name: String,
    pub avatar_url: Option<String>,
}

/// Website profile snapshot, resolved once per connection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub principal: Principal,
    pub name: String,
    pub avatar_url: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Encounter {
    pub player: Player,
    pub peers: Vec<Player>,
}
