use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};

use crate::actors::RoomType;
use crate::actors::messages::{ActorId, ClientMessage, RoomManagerCommand};
use crate::actors::rooms::RoomHandle;
use crate::actors::rooms::global::GlobalRoomActor;
use study_server::AppError;

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

    pub async fn run(mut self) {
        while let Some(cmd) = self.receiver.recv().await {
            match cmd {
                RoomManagerCommand::Join {
                    room_type,
                    room_id,
                    user_id : _,
                    tx : _,
                    reply,
                } => {
                    // 1. check if the room already exists
                    if let Some(handle) = self.rooms.get(&room_id) {
                        // return handle
                        // TODO : actual room joining should be requested by the user after receiving the handle
                        let _ = reply.send(Ok(handle.clone()));
                    } else {
                        // 2. create new room
                        println!("RoomManager: Creating new room '{}'", room_id);
                        // TODO : make RoomType
                        let (room_actor, room_handle) = match room_type {
                            RoomType::Global => GlobalRoomActor::new(), // 기본 로직
                            _ => GlobalRoomActor::new(),                // TODO : other room types
                        };

                        // run the room actor in a separate task
                        tokio::spawn(async move {
                            room_actor.run().await;
                        });

                        self.rooms.insert(room_id.clone(), room_handle.clone());
                        let _ = reply.send(Ok(room_handle));
                    }
                }
            }
        }
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
        user_id: ActorId,
        tx: mpsc::Sender<ClientMessage>,
    ) -> Result<RoomHandle, AppError> {
        let (reply_tx, reply_rx) = oneshot::channel();

        let _ = self
            .sender
            .send(RoomManagerCommand::Join {
                room_type,
                room_id,
                user_id,
                tx,
                reply: reply_tx,
            })
            .await;

        reply_rx.await.map_err(|e| AppError::RoomManager {
            reason: format!("No reply from manager - {}", e).into(),
        })?
    }
}
