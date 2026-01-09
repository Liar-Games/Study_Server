use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::actors::RoomType;
use crate::actors::messages::RoomManagerCommand;
use crate::actors::rooms::RoomHandle;
use crate::actors::rooms::global::GlobalRoomActor;
use crate::actors::rooms::ingame::InGameRoomActor;
use study_server::{AppError, Result};

/// [Actor] Room Manager
pub struct RoomManager {
    /// running rooms map (room_id -> RoomHandle)
    rooms: HashMap<String, RoomHandle>,
    receiver: mpsc::Receiver<RoomManagerCommand>,
    self_handle : RoomManagerHandle,
}

impl RoomManager {
    pub fn new() -> (Self, RoomManagerHandle) {
        let (tx, rx) = mpsc::channel(64);
        let handle = RoomManagerHandle { sender: tx };
        let actor = Self {
            rooms: HashMap::new(),
            receiver: rx,
            self_handle: handle.clone(),
        };
        (actor, handle)
    }

    pub async fn run(mut self) -> Result<()> {
        println!("Room Manager Started");
        while let Some(cmd) = self.receiver.recv().await {
            match cmd {
                /*
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
                */
                RoomManagerCommand::CreateRoom {room_type, room_id, reply} => {
                    let room_id = room_id.unwrap_or_else(|| Uuid::new_v4().to_string());
                    
                    if self.rooms.contains_key(&room_id) {
                         let _ = reply.send(Err(AppError::RoomManager { 
                             reason: format!("Room {} already exists", room_id).into() 
                         }));
                    } else {
                        println!("RoomManager: Creating room '{}' ({:?})", room_id, room_type);
                        
                        let (handle, join_handle) = match room_type {
                            RoomType::Global => {
                                let (actor, handle) = GlobalRoomActor::new(room_id.clone());
                                (handle, tokio::spawn(async move { let _ = actor.run().await; }))
                            },
                            RoomType::InGame => {
                                let (actor, handle) = InGameRoomActor::new(room_id.clone(), self.self_handle.clone());
                                (handle, tokio::spawn(async move { let _ = actor.run().await; }))
                            },
                        };

                        self.rooms.insert(room_id.clone(), handle.clone());
                        let _ = reply.send(Ok((room_id, handle)));
                    }
                },

                RoomManagerCommand::GetRoom { room_id, reply } => {
                    if let Some(handle) = self.rooms.get(&room_id) {
                        let _ = reply.send(Ok(handle.clone()));
                    } else {
                        let _ = reply.send(Err(AppError::RoomManager { 
                            reason: format!("Room {} not found", room_id).into() 
                        }));
                    }
                },

                RoomManagerCommand::DeleteRoom { room_id } =>{
                    if self.rooms.remove(&room_id).is_some() {
                        println!("RoomManager: Room '{}' deleted. remain : {}", room_id, self.rooms.len());
                    } else {
                        eprintln!("RoomManager: Create delete request for unknown room '{}'", room_id);
                    }
                },
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
    /*
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
    */
    pub async fn create_room(&self, room_type: RoomType, room_id: Option<String>) -> Result<(String, RoomHandle)> {
        let (tx, rx) = oneshot::channel();
        self.sender.send(RoomManagerCommand::CreateRoom { room_type, room_id, reply: tx })
            .await.map_err(|_| AppError::RoomManager { reason: "mailbox closed".into() })?;
        rx.await.map_err(|e| AppError::RoomManager { reason: format!("no reply: {}", e).into() })?
    }

    pub async fn get_room(&self, room_id: String) -> Result<RoomHandle> {
        let (tx, rx) = oneshot::channel();
        self.sender.send(RoomManagerCommand::GetRoom { room_id, reply: tx })
            .await.map_err(|_| AppError::RoomManager { reason: "mailbox closed".into() })?;
        rx.await.map_err(|e| AppError::RoomManager { reason: format!("no reply: {}", e).into() })?
    }

    pub async fn delete_room(&self, room_id: String) -> Result<()> {
         let _ = self.sender.send(RoomManagerCommand::DeleteRoom { room_id }).await;
         Ok(())
    }
}
