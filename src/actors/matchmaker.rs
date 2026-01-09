use std::collections::VecDeque;
use tokio::sync::mpsc;
use crate::actors::messages::{ActorId, ClientMessage, MatchmakerCommand};
use crate::actors::room_manager::RoomManagerHandle;
use crate::actors::RoomType;
use study_server::Result;

pub struct Matchmaker {
    receiver: mpsc::Receiver<MatchmakerCommand>,
    room_manager: RoomManagerHandle,
    queue: VecDeque<(ActorId, mpsc::Sender<ClientMessage>)>,
}

impl Matchmaker {
    pub fn new(room_manager: RoomManagerHandle) -> (Self, MatchmakerHandle) {
        let (tx, rx) = mpsc::channel(32);
        let actor = Self {
            receiver: rx,
            room_manager,
            queue: VecDeque::new(),
        };
        (actor, MatchmakerHandle { sender: tx })
    }

    pub async fn run(mut self) {
        println!("Matchmaker Started");
        loop {
            if let Some(cmd) = self.receiver.recv().await {
                match cmd {
                    MatchmakerCommand::JoinQueue { user_id, tx } => {
                        println!("Matchmaker: User {} joined queue. Total : {}", user_id, self.queue.len() + 1);
                        self.queue.push_back((user_id, tx));
                    }
                    MatchmakerCommand::LeaveQueue { user_id } => {
                        // O(n) scan to remove
                        if let Some(pos) = self.queue.iter().position(|(id, _)| id == &user_id) {
                            self.queue.remove(pos);
                        }
                    }
                }
            } else {
                break; // channel closed
            }

            // 2. Matching Logic 
            // TODO : matching by ELO. (current : FIFO)
            while self.queue.len() >= 2 {
                // Extract 2 players
                let (p1_id, p1_tx) = self.queue.pop_front().unwrap();
                let (p2_id, p2_tx) = self.queue.pop_front().unwrap();

                println!("Matchmaker: Matched {} vs {}", p1_id, p2_id);

                // 2-1. Create a new game room
                match self.room_manager.create_room(RoomType::InGame, None).await {
                    Ok((room_id, _room_handle)) => {
                        let _ = p1_tx.send(ClientMessage::JoinRoom { room_id: room_id.clone() }).await;
                        let _ = p2_tx.send(ClientMessage::JoinRoom { room_id: room_id.clone() }).await;
                    }
                    Err(e) => {
                        eprintln!("Matchmaker: Failed to create room: {}", e);
                    }
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct MatchmakerHandle {
    sender: mpsc::Sender<MatchmakerCommand>,
}

impl MatchmakerHandle {
    pub async fn join_queue(&self, user_id: ActorId, tx: mpsc::Sender<ClientMessage>) -> Result<()> {
        self.sender.send(MatchmakerCommand::JoinQueue { user_id, tx })
            .await.map_err(|_| study_server::AppError::Serve { 
                source: std::io::Error::new(std::io::ErrorKind::BrokenPipe, "Matchmaker died") 
            }.into())
    }
}