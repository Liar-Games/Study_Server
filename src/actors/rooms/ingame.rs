use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use std::collections::HashMap;
use bytes::Bytes;
use study_server::{Result, AppError};
use crate::actors::messages::{ActorId, ClientMessage, RoomMessage};
use crate::actors::room_manager::RoomManagerHandle;
use crate::actors::rooms::RoomHandle;

pub struct InGameRoomActor {
    occupants: HashMap<ActorId, mpsc::Sender<ClientMessage>>,
    receiver: mpsc::Receiver<RoomMessage>,
    self_sender: mpsc::Sender<RoomMessage>,
    manager_handle: RoomManagerHandle,
    room_id: String,
    game_started: bool,
}

impl InGameRoomActor {
    pub fn new(room_id: String, manager_handle: RoomManagerHandle) -> (Self, RoomHandle) {
        let (tx, rx) = mpsc::channel(32);
        let actor = Self {
            occupants: HashMap::new(),
            receiver: rx,
            self_sender: tx.clone(),
            manager_handle,
            room_id: room_id.clone(),
            game_started: false,
        };
        (actor, RoomHandle { sender: tx, room_id })
    }

    pub async fn run(mut self) -> Result<()> {
        // [Main Loop]
        loop {
            tokio::select! {
                Some(cmd) = self.receiver.recv() => {
                    self.handle_command(cmd).await?;
                }
                // Add other timers or events here if needed
            }
        }
    }

    async fn handle_command(&mut self, cmd: RoomMessage) -> Result<()> {
        match cmd {
            RoomMessage::Join { user_id, tx } => {
                self.occupants.insert(user_id.clone(), tx);
                println!("GameRoom {}: User {} joined. ({}/2)", self.room_id, user_id, self.occupants.len());

                // start game when 2 players joined
                if self.occupants.len() == 2 && !self.game_started {
                    self.game_started = true;
                    self.start_game_logic();
                }
            }
            RoomMessage::Leave { user_id } => {
                self.occupants.remove(&user_id);
                // logic for when a user leaves (e.g., end game, notify others) can be added here
            }
            RoomMessage::GamePacket { .. } => {
                // Handle game packets here
            },
            RoomMessage::EndGame => {
                println!("GameRoom {}: Cleanup sequence initiated.", self.room_id);
                
                for (uid, tx) in &self.occupants {
                    let _ = tx.send(ClientMessage::ClosedRoom { room_id: self.room_id.clone() }).await;
                }
                self.occupants.clear();

                // You may clean up resources here

                let _ = self.manager_handle.delete_room(self.room_id.clone()).await;

                return Ok(());
            }
        }
        Ok(())
    }

    fn start_game_logic(&self) {
        // clone occupants and room_id for the async task
        let players: Vec<(String, mpsc::Sender<ClientMessage>)> = self.occupants
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        
        let room_id = self.room_id.clone();
        let self_sender = self.self_sender.clone();

        // you may implement game logic here
        tokio::spawn(async move {
            println!("GameRoom {}: Game Started! Waiting 3s...", room_id);
            sleep(Duration::from_secs(3)).await;

            // Now: players[0] wins, players[1] loses after 3 seconds
            if players.len() >= 2 {
                let (p1_id, p1_tx) = &players[0];
                let (p2_id, p2_tx) = &players[1];

                let _ = p1_tx.send(ClientMessage::SendText(format!("GAME_RESULT: WIN (vs {})", p2_id))).await;
                let _ = p2_tx.send(ClientMessage::SendText(format!("GAME_RESULT: LOSE (vs {})", p1_id))).await;
                
                println!("GameRoom {}: Game Finished.", room_id);
                let _ = self_sender.send(RoomMessage::EndGame).await;
            }
            
            // TODO: handle clean up, etc.
        });
    }
}