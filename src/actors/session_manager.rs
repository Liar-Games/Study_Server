use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};

use crate::actors::messages::{ActorId, ClientMessage, ServerMessage};
use study_server::{AppError, Result};
use study_server::error::SessionSendError;

/// [Actor] Session Manager
pub struct SessionManager {
    /// session list: user_id -> client mailbox sender
    sessions: HashMap<ActorId, mpsc::Sender<ClientMessage>>,
    receiver: mpsc::Receiver<ServerMessage>,
}

impl SessionManager {
    /// Constructor
    pub fn new() -> (Self, SessionHandle) {
        // message queue size = 64
        // TODO: drop logic when full
        let (tx, rx) = mpsc::channel(64);

        let actor = Self {
            sessions: HashMap::new(),
            receiver: rx,
        };

        let handle = SessionHandle { sender: tx };
        (actor, handle)
    }

    /// Main Loop
    pub async fn run(mut self) -> Result<()> {
        while let Some(cmd) = self.receiver.recv().await {
            // Policy: per-message failure should not crash the manager loop.
            if let Err(e) = self.handle_message(cmd).await {
                eprintln!("SessionManager error: {}", e);
            }
        }
        Ok(())
    }

    /// message handler
    async fn handle_message(&mut self, cmd: ServerMessage) -> Result<()> {
        match cmd {
            ServerMessage::Connect {
                actor_id,
                tx,
                reply,
            } => {
                // already logged in
                if self.sessions.contains_key(&actor_id) {
                    if reply
                        .send(Err(AppError::DuplicateLogin {
                            user_id: actor_id.clone(),
                        }))
                        .is_err()
                    {
                        eprintln!(
                            "SessionManager: requester dropped (DuplicateLogin) for user_id={}",
                            actor_id
                        );
                    }
                } else {
                    // Insert first (so manager state is consistent), but if requester dropped,
                    // immediately clean up to avoid leaving stale sessions behind.
                    self.sessions.insert(actor_id.clone(), tx);

                    println!(
                        "Manager: New User {} connected. Total: {}",
                        actor_id,
                        self.sessions.len()
                    );

                    if reply.send(Ok(())).is_err() {
                        eprintln!(
                            "SessionManager: requester dropped (Connect) for user_id={}",
                            actor_id
                        );
                        // Clean up: client already gone, so remove the session.
                        self.sessions.remove(&actor_id);
                    }
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
                    // If send fails, the target client mailbox is closed.
                    // Remove stale session entry to prevent repeated failures later.
                    if client_tx.send(msg).await.is_err() {
                        self.sessions.remove(&target_id);
                        return Err(AppError::SessionSendFailed {
                            user_id: target_id.clone(),
                            source: SessionSendError,
                        });
                    }
                } else {
                    println!("Manager: User {} not found.", target_id);
                }
            }

            ServerMessage::GetOnlineCount { reply } => {
                if reply.send(self.sessions.len()).is_err() {
                    eprintln!("SessionManager: requester dropped (GetOnlineCount)");
                }
            }
        }
        Ok(())
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
    ) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();

        let msg = ServerMessage::Connect {
            actor_id,
            tx,
            reply: reply_tx,
        };

        self.sender
            .send(msg)
            .await
            .map_err(|_| AppError::SessionManager {
                reason: "session manager mailbox closed".into(),
            })?;

        // wait for manager reply
        reply_rx.await.map_err(|e| AppError::SessionManager {
            reason: format!("No reply from manager - {}", e).into(),
        })?
    }

    pub async fn disconnect(&self, actor_id: String) -> Result<()> {
        self.sender
            .send(ServerMessage::Disconnect { actor_id })
            .await
            .map_err(|_| AppError::SessionManager {
                reason: "session manager mailbox closed".into(),
            })
    }
}
