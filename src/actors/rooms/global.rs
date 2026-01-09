
///
/// This is a global room that every connected user joins upon login.
/// This room has simple examples for handling game logic.
/// It maintains a list of occupants and processes game packets sent by users.
/// You may make room actors based on their complex game logic from this example.
///


use tokio::sync::mpsc;
use std::collections::HashMap;

use bytes::Bytes;
use study_server::{AppError, Result};
use crate::actors::messages::{ActorId, ClientMessage, RoomMessage};
use study_server::error::SessionSendError;
use super::RoomHandle;

/// [Actor] Room Actor
pub struct GlobalRoomActor {
    /// clients map
    occupants: HashMap<ActorId, mpsc::Sender<ClientMessage>>,
    
    /// Receiver mailbox
    receiver: mpsc::Receiver<RoomMessage>,

    /////////////
    /// 
    /// Room's data here.
    /// For example, a simple shared counter.
    /// 
    /// /////////
    shared_counter: u32,
}

impl GlobalRoomActor {
    pub fn new(room_id: String) -> (Self, RoomHandle) {
        // message queue size = 128
        // TODO : drop logic when full
        let (tx, rx) = mpsc::channel(128);
        
        let actor = Self {
            occupants: HashMap::new(),
            receiver: rx,
            shared_counter: 0,
        };
        
        let handle = RoomHandle { sender: tx, room_id };
        (actor, handle)
    }

    pub async fn run(mut self) -> Result<()> {
        while let Some(cmd) = self.receiver.recv().await {
            if let Err(e) = self.handle_command(cmd).await {
                eprintln!("GlobalRoomActor error: {}", e);
            }
        }
        Ok(())
    }

    async fn handle_command(&mut self, cmd: RoomMessage) -> Result<()> {
        match cmd {
            RoomMessage::Join { user_id, tx } => {
                self.occupants.insert(user_id.clone(), tx);
                println!("GlobalRoom: User {} joined. Total: {}", user_id, self.occupants.len());
            }
            
            RoomMessage::Leave { user_id } => {
                self.occupants.remove(&user_id);
                println!("GlobalRoom: User {} left.", user_id);
            }
            
            RoomMessage::GamePacket { user_id, data } => {
                self.handle_game_logic(user_id, data).await?;
            }
            RoomMessage::EndGame => {
                println!("GlobalRoom: EndGame");
                // cleanup logic
                // in common situation, global room should not be ended.
            }
        }
        Ok(())
    }

    /// Helper functions for messaging
    /// 1. Unicast
    async fn unicast(&self, target_id: &ActorId, op_code: u8, payload: &[u8]) -> Result<()> {
        if let Some(tx) = self.occupants.get(target_id) {
            let packet = self.make_packet(op_code, payload);
            tx.send(ClientMessage::SendBinary(packet))
                .await
                .map_err(|_| AppError::SessionSendFailed {
                    user_id: target_id.clone(),
                    source: SessionSendError,
                })?;
        }
        Ok(())
    }

    /// 2. Broadcast
    /// - exclude_id: Some(id) to exclude specific user from broadcast (e.g., sender itself)
    async fn broadcast(&self, op_code: u8, payload: &[u8], exclude_id: Option<&ActorId>) -> Result<()> {
        let packet = self.make_packet(op_code, payload);
        
        for (uid, tx) in &self.occupants {
            if let Some(ex_id) = exclude_id {
                if uid == ex_id { continue; }
            }
            // TODO : clone okay??
            tx.send(ClientMessage::SendBinary(packet.clone()))
                .await
                .map_err(|_| AppError::SessionSendFailed {
                    user_id: uid.clone(),
                    source: SessionSendError,
                })?;
        }
        Ok(())
    }

    /// 3. Broadcast Text
    async fn broadcast_text(&self, text: String, exclude_id: Option<&ActorId>) -> Result<()> {
        for (uid, tx) in &self.occupants {
            if let Some(ex_id) = exclude_id {
                if uid == ex_id { continue; }
            }
            // TODO : clone okay??
            tx.send(ClientMessage::SendText(text.clone()))
                .await
                .map_err(|_| AppError::SessionSendFailed {
                    user_id: uid.clone(),
                    source: SessionSendError,
                })?;
        }
        Ok(())
    }

    /// helper's helper
    fn make_packet(&self, op_code: u8, payload: &[u8]) -> Vec<u8> {
        let mut packet = Vec::with_capacity(1 + payload.len());
        packet.push(op_code);
        packet.extend_from_slice(payload);
        packet
    }

    ///////////////////
    /// 
    /// implement your game logic in this function.
    /// 
    /// ///////////////

    async fn handle_game_logic(&mut self, user_id: ActorId, data: Bytes) -> Result<()> {
        if data.is_empty() { return Ok(()); }

        let op_code = data[0]; // Game's OpCode
        let payload = &data[1..];

        match op_code {
            // 1. Unicast Example: add 1 and return to sender
            1 => {
                let mut response = payload.to_vec();
                for byte in &mut response {
                    *byte = byte.wrapping_add(1);
                }
                self.unicast(&user_id, 1, &response).await?;
            },

            // 2. Broadcast Example: relay to all occupants
            2 => {
                self.broadcast(2, payload, Some(&user_id)).await?;
            },

            // 3. Room Memory change Example: add counter and notify all occupants
            3 => {
                self.shared_counter += 1;
                let msg = format!("Room Counter: {}", self.shared_counter);
                
                self.broadcast_text(msg, None).await?;
            }

            // TODO : AppError
            _ => println!("Unknown Game OpCode: {}", op_code),
        }
        Ok(())
    }
}
