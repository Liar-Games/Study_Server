use tokio::sync::mpsc;
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
}

impl RoomHandle {
    pub async fn join(&self, user_id: ActorId, tx: mpsc::Sender<ClientMessage>) {
        let _ = self.sender.send(RoomMessage::Join { user_id, tx }).await;
    }
    
    pub async fn leave(&self, user_id: ActorId) {
        let _ = self.sender.send(RoomMessage::Leave { user_id }).await;
    }
    
    pub async fn send_packet(&self, user_id: ActorId, data: Vec<u8>) {
        let _ = self.sender.send(RoomMessage::GamePacket { user_id, data }).await;
    }
}