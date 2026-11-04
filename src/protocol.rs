//! Wire version 1. JSON is used only for infrequent control traffic.
use crate::{
    Error, Result,
    types::{Player, RoomId},
};
use bytes::{BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};

pub const AUTH: u8 = 1;
pub const WELCOME: u8 = 2;
pub const COMMAND: u8 = 3;
pub const REPLY: u8 = 4;
pub const ROOM_CLOSED: u8 = 5;
pub const GAME: u8 = 10;
pub const ERROR: u8 = 255;
pub const GAME_HEADER: usize = 11;

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthRequest {
    Guest {
        version: u8,
    },
    Ticket {
        version: u8,
        ticket: String,
    },
    Resume {
        version: u8,
        session_id: crate::types::SessionId,
        token: String,
        #[serde(default)]
        ticket: Option<String>,
    },
}
impl AuthRequest {
    pub fn version(&self) -> u8 {
        match self {
            Self::Guest { version }
            | Self::Ticket { version, .. }
            | Self::Resume { version, .. } => *version,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Create { capacity: usize },
    Join { room_id: RoomId },
    Leave { room_id: RoomId },
    Queue,
    CancelQueue,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct Request {
    pub request_id: u32,
    #[serde(flatten)]
    pub operation: Operation,
}
#[derive(Debug, Serialize)]
pub struct Welcome<'a> {
    pub version: u8,
    pub player: &'a Player,
    pub global_room: RoomId,
    pub resume_token: &'a str,
    pub resumed: bool,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Reply {
    pub request_id: Option<u32>,
    pub room_id: Option<RoomId>,
    pub error: Option<String>,
}
impl Reply {
    pub fn new(request_id: Option<u32>, result: Result<Option<RoomId>>) -> Self {
        match result {
            Ok(room_id) => Self {
                request_id,
                room_id,
                error: None,
            },
            Err(error) => Self {
                request_id,
                room_id: None,
                error: Some(
                    serde_json::to_value(error)
                        .expect("error enum")
                        .as_str()
                        .expect("string")
                        .to_owned(),
                ),
            },
        }
    }
}
pub fn json(tag: u8, value: &impl Serialize) -> Bytes {
    let mut data = vec![tag];
    serde_json::to_writer(&mut data, value).expect("protocol values are serializable");
    data.into()
}

#[derive(Clone, Debug)]
pub struct GameFrame {
    pub room_id: RoomId,
    pub opcode: u16,
    pub payload: Bytes,
}
impl GameFrame {
    pub fn decode(data: Bytes) -> Result<Self> {
        if data.len() < GAME_HEADER || data[0] != GAME {
            return Err(Error::InvalidPacket);
        }
        Ok(Self {
            room_id: u64::from_be_bytes(data[1..9].try_into().unwrap()),
            opcode: u16::from_be_bytes(data[9..11].try_into().unwrap()),
            payload: data.slice(GAME_HEADER..),
        })
    }
    pub fn encode(&self) -> Bytes {
        let mut data = BytesMut::with_capacity(GAME_HEADER + self.payload.len());
        data.put_u8(GAME);
        data.put_u64(self.room_id);
        data.put_u16(self.opcode);
        data.extend_from_slice(&self.payload);
        data.freeze()
    }
}
