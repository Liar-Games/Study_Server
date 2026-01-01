use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};

use crate::actors::RoomType;
use crate::actors::messages::RoomManagerCommand;
use crate::actors::rooms::RoomHandle;
use crate::actors::rooms::global::GlobalRoomActor;
use study_server::{AppError, Result};

/// [Actor] Room Manager
pub struct RoomManager {
    /// running rooms map (room_id -> RoomHandle)
    rooms: HashMap<String, RoomHandle>,
    receiver: mpsc::Receiver<RoomManagerCommand>,
}

impl RoomManager {
    pub fn new() -> (Self, RoomManagerHandle) {
        let (tx, rx) = mpsc::channel(64);
        let actor = Self {
            rooms: HashMap::new(),
            receiver: rx,
        };
        (actor, RoomManagerHandle { sender: tx })
    }

    pub async fn run(mut self) -> Result<()> {
        while let Some(cmd) = self.receiver.recv().await {
            match cmd {
                RoomManagerCommand::Join {
                    room_type,
                    room_id,
                    reply,
                } => {
                    // 1. check if the room already exists
                    if let Some(handle) = self.rooms.get(&room_id) {
                        if reply.send(Ok(handle.clone())).is_err() {
                            eprintln!("RoomManager: requester dropped for room_id={}", room_id);
                        }
                    } else {
                        // 2. create new room
                        println!("RoomManager: Creating new room '{}'", room_id);
                        // TODO : make RoomType
                        let (room_actor, room_handle) = match room_type {
                            RoomType::Global => GlobalRoomActor::new(room_id.clone()), // 기본 로직
                            _ => GlobalRoomActor::new(room_id.clone()),                // TODO : other room types
                        };

                        // run the room actor in a separate task
                        tokio::spawn(async move {
                            if let Err(e) = room_actor.run().await {
                                eprintln!("Room actor exited with error: {}", e);
                            }
                        });

                        self.rooms.insert(room_id.clone(), room_handle.clone());
                        if reply.send(Ok(room_handle)).is_err() {
                            eprintln!("RoomManager: requester dropped after creating room_id={}", room_id);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// handler to communicate with Room Manager Actor
#[derive(Clone)]
pub struct RoomManagerHandle {
    sender: mpsc::Sender<RoomManagerCommand>,
}

impl RoomManagerHandle {
    pub async fn join_room(
        &self,
        room_type: RoomType,
        room_id: String,
    ) -> Result<RoomHandle> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let room_id_clone = room_id.clone();

        self.sender
            .send(RoomManagerCommand::Join {
                room_type,
                room_id,
                reply: reply_tx,
            })
            .await
            .map_err(|_| AppError::RoomManager {
                reason: "room manager mailbox closed".into(),
            })?;

        reply_rx.await.map_err(|e| AppError::RoomManager {
            reason: format!("room manager dropped reply for room_id={} - {}", room_id_clone, e).into(),
        })?
    }
}
