use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};

use crate::actors::messages::{ActorId, ClientMessage, ServerMessage};
use study_server::AppError;

/// [Actor] Session Manager
pub struct SessionManager {
    /// session list
    sessions: HashMap<ActorId, mpsc::Sender<ClientMessage>>,
    receiver: mpsc::Receiver<ServerMessage>,
}

impl SessionManager {
    /// Constructor
    pub fn new() -> (Self, SessionHandle) {
        // message queue size = 64
        // TODO : drop logic when full
        // Gemini : make multiple session manager and load balancer when you need more scalability
        let (tx, rx) = mpsc::channel(64);

        let actor = Self {
            sessions: HashMap::new(),
            receiver: rx,
        };

        let handle = SessionHandle { sender: tx };

        (actor, handle)
    }

    /// Main Loop
    pub async fn run(mut self) {
        while let Some(cmd) = self.receiver.recv().await {
            self.handle_message(cmd).await;
        }
    }

    /// message handler
    // TODO : AppError
    async fn handle_message(&mut self, cmd: ServerMessage) {
        match cmd {
            ServerMessage::Connect {
                actor_id,
                tx,
                reply,
            } => {
                // already logged in
                if self.sessions.contains_key(&actor_id) {
                    let _ = reply.send(Err(AppError::DuplicateLogin {
                        user_id: actor_id.clone(),
                    }));
                } else {
                    self.sessions.insert(actor_id.clone(), tx);
                    println!(
                        "Manager: New User {} connected. Total: {}",
                        actor_id,
                        self.sessions.len()
                    );
                    let _ = reply.send(Ok(()));
                }
            }
            ServerMessage::Disconnect { actor_id } => {
                if self.sessions.remove(&actor_id).is_some() {
                    println!(
                        "Manager: User {} disconnected. Total: {}",
                        actor_id,
                        self.sessions.len()
                    );
                }
            }

            ServerMessage::MessageTo { target_id, msg } => {
                if let Some(client_tx) = self.sessions.get(&target_id) {
                    let _ = client_tx.send(msg).await;
                } else {
                    println!("Manager: User {} not found.", target_id);
                }
            }

            ServerMessage::GetOnlineCount { reply } => {
                let _ = reply.send(self.sessions.len());
            }
        }
    }
}

/// [Handle] Session Manager Handler
/// each user actor get (clone) this to communicate with the session manager
#[derive(Clone)]
pub struct SessionHandle {
    sender: mpsc::Sender<ServerMessage>,
}

impl SessionHandle {
    pub async fn connect(
        &self,
        actor_id: String,
        tx: mpsc::Sender<ClientMessage>,
    ) -> Result<(), AppError> {
        let (reply_tx, reply_rx) = oneshot::channel();

        let msg = ServerMessage::Connect {
            actor_id: actor_id,
            tx,
            reply: reply_tx,
        };

        if self.sender.send(msg).await.is_err() {
            return Err(AppError::SessionManager {
                reason: format!("Session manager is dead").into(),
            });
        }

        // wait for manager reply
        reply_rx.await.map_err(|e| AppError::SessionManager {
            reason: format!("No reply from manager - {}", e).into(),
        })?
    }

    pub async fn disconnect(&self, actor_id: String) {
        let _ = self
            .sender
            .send(ServerMessage::Disconnect { actor_id })
            .await;
    }
}
