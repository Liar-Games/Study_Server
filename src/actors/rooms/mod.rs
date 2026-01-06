use tokio::sync::mpsc;
use bytes::Bytes;
use study_server::{AppError, Result};
use crate::actors::messages::{ActorId, ClientMessage, RoomMessage};


pub mod global;
// you may add more room actors here

// roomtype example
#[derive(Debug)]
pub enum RoomType {
    Global,
    RoomExample, // you may add more room types here
}

/// handler to communicate with Room Actor
#[derive(Clone, Debug)]
pub struct RoomHandle {
    sender: mpsc::Sender<RoomMessage>,
    room_id: String,
}

impl RoomHandle {
    pub async fn join(&self, user_id: ActorId, tx: mpsc::Sender<ClientMessage>) -> Result<()> {
        self.sender
            .send(RoomMessage::Join { user_id, tx })
            .await
            .map_err(|_| {
                AppError::RoomManager {
                    reason: format!("room mailbox closed: {}", self.room_id).into(),
                }
            })
    }
    
    pub async fn leave(&self, user_id: ActorId) -> Result<()> {
        self.sender
            .send(RoomMessage::Leave { user_id })
            .await
            .map_err(|_| {
                AppError::RoomManager {
                    reason: format!("room mailbox closed: {}", self.room_id).into(),
                }
            })
    }
    
    pub async fn send_packet(&self, user_id: ActorId, data: Bytes) -> Result<()> {
        self.sender
            .send(RoomMessage::GamePacket { user_id, data })
            .await
            .map_err(|_| {
                AppError::RoomManager {
                    reason: format!("room mailbox closed: {}", self.room_id).into(),
                }
            })
    }
}
