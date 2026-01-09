use std::collections::HashMap;

use axum::extract::ws::{Message, WebSocket};
use bytes::Bytes;
use futures::{sink::SinkExt, stream::StreamExt};
use redis::AsyncCommands;
use study_server::{AppError, Result};
use tokio::sync::mpsc;

use crate::actors::PacketTag;
use crate::actors::RoomType;
use crate::actors::matchmaker::MatchmakerHandle;
use crate::actors::messages::ClientMessage;
use crate::actors::room_manager::RoomManagerHandle;
use crate::actors::rooms::RoomHandle;
use crate::actors::session_manager::SessionHandle;

/// [Actor] Client Actor
/// each connected user is one ClientActor
pub struct ClientActor {
    /// mailbox to receive messages (from server)
    receiver: mpsc::Receiver<ClientMessage>,
    sender: mpsc::Sender<ClientMessage>,

    /// WebSocket Write
    ws_sender: futures::stream::SplitSink<WebSocket, Message>,

    /// WebSocket Read
    ws_receiver: futures::stream::SplitStream<WebSocket>,

    /// session manager handler
    session_handle: SessionHandle,

    /// room handler
    room_manager: RoomManagerHandle,

    /// matchmaker handler
    matchmaker: MatchmakerHandle,

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
        matchmaker: MatchmakerHandle,
    ) {
        let (ws_sender, ws_receiver) = socket.split();

        // message queue size = 32
        // TODO : drop logic when full
        let (tx, rx) = mpsc::channel(32);

        let mut actor = Self {
            receiver: rx,
            sender: tx.clone(),
            ws_sender,
            ws_receiver,
            session_handle,
            room_manager,
            matchmaker,
            joined_rooms: HashMap::new(),
            redis_client,
            id: String::new(),
            is_guest: false,
        };

        tokio::spawn(async move {
            // 1. Authenticate
            // TODO : tx.clone() okay??
            if let Err(e) = actor.authenticate().await {
                eprintln!("Authentication failed: {}", e);
                let _ = actor.ws_sender.send(Message::Close(None)).await;
                return;
            }

            // 2. Main Loop
            if let Err(e) = actor.run().await {
                eprintln!("Client actor main loop error: {}", e);
                // best-effort close; ignore error because socket may already be dead
                let _ = actor.ws_sender.send(Message::Close(None)).await;
            }

            // 3. Cleanup
            if let Err(e) = actor.cleanup().await {
                eprintln!("Cleanup error: {}", e);
            }
        });
    }

    /// 1. Authenticate
    async fn authenticate(&mut self) -> Result<()> {
        // our websocket handshake -> Must receive a Ticket packet first
        match self.ws_receiver.next().await {
            Some(Ok(Message::Binary(bytes))) => {
                if bytes.is_empty() || bytes[0] != PacketTag::Auth as u8 {
                    return Err(AppError::InvalidFrame {
                        reason: "First frame must be auth".into(),
                    });
                }

                if bytes.len() < 2 {
                    // Empty ticket -> guest user
                    self.is_guest = true;
                    // TODO : make unique guest id
                    // guest id format will change. see below user_id format's TODO.
                    self.id = "{\"id\":1557, \"name\":\"Guest_1557\"}".to_string();

                    self.login_process().await?;
                    self.send_packet(PacketTag::Text, b"Guest Login Success")
                        .await?;
                    return Ok(());
                }

                // Ticket Parse
                let ticket =
                    String::from_utf8(bytes[1..].to_vec()).map_err(|e| AppError::Utf8 {
                        field: "ticket".into(),
                        source: e,
                    })?;

                // Check Ticket in Redis
                // TODO (LATER) : make redis connection every time is inefficient. use connection pool. (redis)
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

                self.login_process().await?;
                self.send_packet(PacketTag::Text, b"Login Success").await?;
                Ok(())
            }
            Some(Ok(Message::Text(_))) => Err(AppError::WebSocket {
                reason: "Text frame not supported".into(),
            }),
            Some(Ok(Message::Close(_))) | None => Err(AppError::WebSocket {
                reason: "Closed before auth".into(),
            }),
            Some(Ok(_)) => Err(AppError::WebSocket {
                reason: "Unsupported websocket frame (expected binary)".into(),
            }),
            Some(Err(e)) => Err(AppError::WebSocket {
                reason: format!("recv - {}", e).into(),
            }),
        }
    }

    // helper for login process
    async fn login_process(&mut self) -> Result<()> {
        self.session_handle
            .connect(self.id.clone(), self.sender.clone())
            .await
            .map_err(|e| AppError::SessionManager {
                reason: format!("Manager connect failed: {}", e).into(),
            })?;
        // josin default "global" room
        // TODO : join global room
        self.join_room(RoomType::Global, "global".to_string()).await?;
        Ok(())
    }
    /// 2. Main Loop
    async fn run(&mut self) -> Result<()> {
        loop {
            tokio::select! {
                // A. Get Messages from Server
                recv = self.receiver.recv() => {
                    match recv {
                        Some(msg) => {
                            match msg {
                                ClientMessage::SendText(text) => {
                                    self.send_packet(PacketTag::Text, text.as_bytes()).await?;
                                }
                                ClientMessage::SendBinary(data) => {
                                    self.send_packet(PacketTag::Binary, &data).await?;
                                }
                                ClientMessage::Kick { reason } => {
                                    self.send_packet(PacketTag::TextError, format!("Kicked: {}", reason).as_bytes()).await?;
                                    break; // Loop exit -> connection closed
                                }
                                ClientMessage::JoinRoom { room_id } => {
                                    println!("Client {}: Match Found! Join room {}", self.id, room_id);
                                    if let Err(e) = self.join_room(RoomType::InGame, room_id).await {
                                         let _ = self.send_packet(PacketTag::TextError, format!("Match Join Error: {}", e).as_bytes()).await;
                                    }
                                }
                                ClientMessage::ClosedRoom {room_id} => {
                                    self.joined_rooms.remove(&room_id);
                                    println!("Client {}: Left Room {}", self.id, room_id);
                                    let _ = self.send_packet(PacketTag::Text, format!("ROOM_CLOSED:{}", room_id).as_bytes()).await;
                                }
                            }
                        }
                        None => {
                            eprintln!("ClientActor receiver closed for {}", self.id);
                            let _ = self.ws_sender.send(Message::Close(None)).await;
                            break;
                        }
                    }
                }

                // B. Get Messages from WebSocket
                result = self.ws_receiver.next() => {
                    match result {
                        Some(Ok(msg)) => {
                            match msg {
                                Message::Binary(data) => self.handle_binary(data.into()).await?,
                                Message::Close(_) => break,
                                Message::Ping(_) | Message::Pong(_) => { /* keep-alive frames */ }
                                _ => {
                                    return Err(AppError::WebSocket {
                                        reason: "Unsupported frame type".into(),
                                    });
                                }
                            }
                        }
                        Some(Err(e)) => {
                            return Err(AppError::WebSocket { reason: format!("recv - {}", e).into() });
                        }
                        None => {
                            // client closed connection
                            break;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// 3. Handle Binary Packets (Heartbeat, Game Logic, etc.)
    async fn handle_binary(&mut self, data: Bytes) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }

        match PacketTag::from_u8(data[0]) {
            Some(PacketTag::Heartbeat) => {
                self.send_packet(PacketTag::Heartbeat, &[]).await?;
            }
            Some(PacketTag::Game) => {
                if data.len() > 1 {
                    let game_payload = data.slice(1..);
                    if let Some(room) = self.joined_rooms.get("global") {
                        room.send_packet(self.id.clone(), game_payload).await?;
                    } else {
                        return Err(AppError::RoomManager {
                            reason: "global room handle missing".into(),
                        });
                    }
                }
            }
            Some(PacketTag::JoinQueue) => {
                // TODO : parse payload
                self.matchmaker.join_queue(self.id.clone(), self.sender.clone()).await?;
                self.send_packet(PacketTag::Text, b"Joined Queue").await?;
            }
            // you may add more PacketTag and handle them here
            Some(_) => {
                // Known PacketTag but not handled here (ex: Auth/Text/Binary)
                return Err(AppError::WebSocket {
                    reason: format!("unexpected packet tag in binary handler: {}", data[0]).into(),
                });
            }
            None => {
                return Err(AppError::UnknownSocketType { value: data[0] });
            }
        }
        Ok(())
    }

    /// 3. Cleanup on Disconnect
    async fn cleanup(&mut self) -> Result<()> {
        for (_rid, handle) in &self.joined_rooms {
            if let Err(e) = handle.leave(self.id.clone()).await {
                eprintln!("cleanup leave failed (user {}): {}", self.id, e);
            }
        }
        if let Err(e) = self.session_handle.disconnect(self.id.clone()).await {
            eprintln!("cleanup disconnect failed (user {}): {}", self.id, e);
        }
        println!("ClientActor: {} cleanup done.", self.id);
        Ok(())
    }

    // helper
    // TODO (Later) : send packet every request is inefficient.
    // make buffer and accumulate data.
    // data's priority logic handles in each game-logic actor
    // TODO : making packet vector every send is inefficient. is there zero-copy way?
    async fn send_packet(&mut self, tag: PacketTag, payload: &[u8]) -> Result<()> {
        let mut data = Vec::with_capacity(1 + payload.len());
        data.push(tag as u8);
        data.extend_from_slice(payload);
        self.ws_sender
            .send(Message::Binary(data.into()))
            .await
            .map_err(|e| AppError::WebSocket {
                reason: format!("send - {}", e).into(),
            })
    }

    // TODO : return type refactoring
    async fn join_room(&mut self, room_type: RoomType, room_id: String) -> Result<()> {
        // Get Room Handle
        let handle = match self.room_manager.get_room(room_id.clone()).await {
            Ok(h) => h,
            Err(e) => {
                eprintln!("Client {} : failed to find room {}: {}", self.id, room_id, e);
                self.send_packet(PacketTag::TextError, format!("Room '{}' not found.", room_id).as_bytes()).await?;
                return Ok(());
            }
        };
        // Try join room
        if let Err(e) = handle.join(self.id.clone(), self.sender.clone()).await {
            eprintln!("Client {} : failed to join room actor {}: {}", self.id, room_id, e);
            self.send_packet(PacketTag::TextError, format!("Failed to enter room: {}", e).as_bytes()).await?;
            return Ok(());
        };
        self.joined_rooms.insert(room_id.clone(), handle);
        println!("Client {}: Joined Room '{}'", self.id, room_id);
        Ok(())
    }
}
