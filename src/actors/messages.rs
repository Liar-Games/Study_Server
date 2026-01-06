use tokio::sync::{mpsc, oneshot};
use bytes::Bytes;

/* Custom Libraries */
use study_server::{Result};
use crate::actors::RoomType;
use crate::actors::rooms::RoomHandle;


// Identifier for actors (userID or sessionID for guest)
pub type ActorId = String;

// 1. Send Messages to Client(User)
#[derive(Debug)]
pub enum ClientMessage {
    SendText(String),
    SendBinary(Vec<u8>),
    Kick { reason: String },
}

// 2. Send Messages to Server (SessionManager)
#[derive(Debug)]
pub enum ServerMessage {
    /// Connect Request
    /// - actor_id: userID or None for guest
    /// - tx: Manager->Client channel
    /// - reply: Manager informs the result through this one-time channel
    Connect {
        actor_id: ActorId,
        tx: mpsc::Sender<ClientMessage>,
        reply: oneshot::Sender<Result<()>>, // inform the result
    },

    /// Disconnect Request
    /// Request to remove the user from the manager's list when the user disconnects
    Disconnect { actor_id: ActorId },

    /// Send a message to a specific user
    MessageTo {
        target_id: ActorId,
        msg: ClientMessage,
    },

    /// test message : Get the number of online users
    GetOnlineCount { reply: oneshot::Sender<usize> },
}

// 3. Send Messages to Room (Game Logic Actor)
#[derive(Debug)]
pub enum RoomMessage {
    /// join room
    Join {
        user_id: ActorId,
        tx: mpsc::Sender<ClientMessage>, // channel for the room to send messages to the user
    },
    
    /// leave room
    Leave {
        user_id: ActorId,
    },
    
    /// game data packet (OpCode + Payload)
    GamePacket {
        user_id: ActorId,
        data: Bytes, 
    }
}

// 4. Send Messages to Room Manager Actor 
#[derive(Debug)]
pub enum RoomManagerCommand {
    /// request to create or fetch a room handle
    /// - room_id: name of the room to join (e.g., "global", "game_1")
    /// - reply: channel to receive the room handle
    Join {
        room_type: RoomType,
        room_id: String,
        reply: oneshot::Sender<Result<RoomHandle>>, 
    },
}
