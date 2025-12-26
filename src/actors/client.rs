use std::collections::HashMap;

use axum::extract::ws::{Message, WebSocket};
use futures::{sink::SinkExt, stream::StreamExt};
use redis::AsyncCommands;
use study_server::AppError;
use tokio::sync::mpsc;

use crate::actors::PacketTag;
use crate::actors::RoomType;
use crate::actors::messages::ClientMessage;
use crate::actors::room_manager::RoomManagerHandle;
use crate::actors::rooms::RoomHandle;
use crate::actors::session_manager::SessionHandle;

/// [Actor] Client Actor
/// each connected user is one ClientActor
pub struct ClientActor {
    /// mailbox to receive messages (from server)
    receiver: mpsc::Receiver<ClientMessage>,

    /// WebSocket Write
    ws_sender: futures::stream::SplitSink<WebSocket, Message>,

    /// WebSocket Read
    ws_receiver: futures::stream::SplitStream<WebSocket>,

    /// session manager handler
    session_handle: SessionHandle,

    /// room handler
    room_manager: RoomManagerHandle,

    /// joined rooms map (room_id -> RoomHandle)
    // TODO : HashMap is TOO powerful. use Vec or other structure.
    joined_rooms: HashMap<String, RoomHandle>,

    /// Redis client
    redis_client: redis::Client,

    /// actorId (based on userID)
    id: String,

    // guest user (not logged in user)
    is_guest: bool,
}

impl ClientActor {
    pub async fn start(
        socket: WebSocket,
        session_handle: SessionHandle,
        redis_client: redis::Client,
        room_manager: RoomManagerHandle,
    ) {
        let (ws_sender, ws_receiver) = socket.split();

        // message queue size = 32
        // TODO : drop logic when full
        let (tx, rx) = mpsc::channel(32);

        let mut actor = Self {
            receiver: rx,
            ws_sender,
            ws_receiver,
            session_handle,
            room_manager,
            joined_rooms: HashMap::new(),
            redis_client,
            id: String::new(),
            is_guest: false,
        };

        // Authenticate and run
        // TODO : AppError
        tokio::spawn(async move {
            // 1. Authenticate
            if let Err(e) = actor.authenticate(tx).await {
                eprintln!("Authentication failed: {}", e);
                let _ = actor.ws_sender.send(Message::Close(None)).await;
                return;
            }

            // 2. Main Loop
            actor.run().await;

            // 3. Cleanup
            actor.cleanup().await;
        });
    }

    /// 1. Authenticate
    /// TODO : AppError
    async fn authenticate(&mut self, my_tx: mpsc::Sender<ClientMessage>) -> Result<(), AppError> {
        // our websocket handshake -> Must receive a Ticket packet first
        if let Some(Ok(msg)) = self.ws_receiver.next().await {
            if let Message::Binary(bytes) = msg {
                if bytes.is_empty() || bytes[0] != PacketTag::Auth as u8 {
                    return Err(AppError::InvalidFrame {
                        reason: format!("First packet is expected Ticket").into(),
                    });
                }

                if bytes.len() < 2 {
                    // Empty ticket -> guest user
                    self.is_guest = true;
                    // TODO : make unique guest id
                    // guest id format will change. see below user_id format's TODO.
                    self.id = "{\"id\":1557, \"name\":\"Guest_1557\"}".to_string();

                    self.login_process(my_tx).await?;

                    let _ = self
                        .send_packet(PacketTag::Text, b"Guest Login Success")
                        .await;
                    return Ok(());
                }

                // Ticket Parse
                let ticket =
                    String::from_utf8(bytes[1..].to_vec()).map_err(|e| AppError::Utf8 {
                        field: "ticket".into(),
                        source: e,
                    })?;

                // Check Ticket in Redis
                // TODO (LATER) : make redis connection every time is inefficient. use connection pool.
                let mut conn = self
                    .redis_client
                    .get_multiplexed_async_connection()
                    .await
                    .map_err(|e| AppError::Redis {
                        reason: format!("Redis connection failed: {}", e).into(),
                        source: e,
                    })?;

                // TODO : user_id format is JSON now. parse JSON and make ActorId from it.
                let user_id: String = conn
                    .get_del(format!("game_ticket:{}", ticket))
                    .await
                    .map_err(|_| AppError::InvalidTicket)?;
                self.id = user_id;

                // TODO : maybe game-load from DB here??

                self.login_process(my_tx).await?;
                let _ = self.send_packet(PacketTag::Text, b"Login Success").await;
                return Ok(());
            }
        }
        Err(AppError::SessionManager {
            reason: "Connection closed before auth".into(),
        })
    }

    // helper for login process
    async fn login_process(&mut self, my_tx: mpsc::Sender<ClientMessage>) -> Result<(), AppError> {
        // TODO : my_tx clone okay??
        self.session_handle
            .connect(self.id.clone(), my_tx.clone())
            .await
            .map_err(|e| AppError::SessionManager {
                reason: format!("Manager connect failed: {}", e).into(),
            })?;
        // josin default "global" room
        self.join_room(RoomType::Global, "global".to_string(), my_tx)
            .await;
        Ok(())
    }
    /// 2. Main Loop
    /// TODO : AppError
    async fn run(&mut self) {
        loop {
            tokio::select! {
                // A. Get Messages from Server
                Some(msg) = self.receiver.recv() => {
                    match msg {
                        ClientMessage::SendText(text) => {
                            let _ = self.send_packet(PacketTag::Text, text.as_bytes()).await;
                        }
                        ClientMessage::SendBinary(data) => {
                            let _ = self.send_packet(PacketTag::Binary, &data).await;
                        }
                        ClientMessage::Kick { reason } => {
                            let _ = self.send_packet(PacketTag::TextError, format!("Kicked: {}", reason).as_bytes()).await;
                            break; // Loop exit -> connection closed
                        }
                    }
                }

                // B. Get Messages from WebSocket
                result = self.ws_receiver.next() => {
                    match result {
                        Some(Ok(msg)) => {
                            match msg {
                                Message::Binary(data) => self.handle_binary(data).await,
                                Message::Close(_) => break,
                                _ => {} // TODO : error (websocket data expected binary)
                            }
                        }
                        Some(Err(_)) | None => break, // TODO : AppError
                    }
                }
            }
        }
    }

    /// 3. Handle Binary Packets (Heartbeat, Game Logic, etc.)
    /// TODO : AppError
    async fn handle_binary(&mut self, data: Vec<u8>) {
        if data.is_empty() {
            return;
        }

        match PacketTag::from_u8(data[0]) {
            Some(PacketTag::Heartbeat) => {
                // Heartbeat packets need to keep / check a WebSocket connection.
                // reply empty heartbeat packet.
                let _ = self.send_packet(PacketTag::Heartbeat, &[]).await;
            }
            Some(PacketTag::Game) => {
                if data.len() > 1 {
                    let game_payload = data[1..].to_vec();
                    // TODO : clone okay?
                    // TODO : now using only "global" room. need room selection protocol between joined rooms.
                    self.joined_rooms
                        .get(&"global".to_string())
                        .unwrap()
                        .send_packet(self.id.clone(), game_payload)
                        .await;
                }
            }
            _ => {
                println!("User {:?} sent game data: {:?}", self.id, data);
            }
        }
    }

    /// 3. Cleanup on Disconnect
    async fn cleanup(&self) {
        for (_rid, handle) in &self.joined_rooms {
            handle.leave(self.id.clone()).await;
        }
        self.session_handle.disconnect(self.id.clone()).await;
        println!("ClientActor: {} cleanup done.", self.id);
    }

    // helper
    // TODO (Later) : send packet every request is inefficient.
    // make buffer and accumulate data.
    // this logic handles in each game-logic actor
    // TODO : making packet vector every send is inefficient. is there zero-copy way?
    async fn send_packet(&mut self, tag: PacketTag, payload: &[u8]) -> Result<(), AppError> {
        let mut data = Vec::with_capacity(1 + payload.len());
        data.push(tag as u8);
        data.extend_from_slice(payload);
        self.ws_sender
            .send(Message::Binary(data))
            .await
            .map_err(|e| AppError::WebSocket {
                reason: format!("send - {}", e).into(),
            })
    }

    // helper
    async fn join_room(
        &mut self,
        room_type: RoomType,
        room_id: String,
        my_tx: mpsc::Sender<ClientMessage>,
    ) {
        // request to Room Manager to join a room
        match self
            .room_manager
            .join_room(room_type, room_id.clone(), self.id.clone(), my_tx.clone())
            .await
        {
            Ok(handle) => {
                handle.join(self.id.clone(), my_tx).await;
                self.joined_rooms.insert(room_id, handle);
            }
            Err(e) => eprintln!("Failed to join room: {}", e),
        }
    }
}
