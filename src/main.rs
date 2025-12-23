use axum::{
    Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
    routing::get,
};
use bytes::{BufMut, BytesMut};
use dashmap::DashMap;
use futures::{sink::SinkExt, stream::StreamExt};
use redis::AsyncCommands;
use std::env;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;

type SessionMap = Arc<DashMap<String, mpsc::Sender<Message>>>;

#[derive(Clone)]
pub struct AppState {
    pub redis_client: redis::Client,
    pub sessions: SessionMap,
}

#[tokio::main]
async fn main() {
    let redis_url = env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1/".to_string());
    let redis = redis::Client::open(redis_url).expect("Invalid Redis URL");
    let sessions = Arc::new(DashMap::new());
    let state = AppState {
        redis_client: redis,
        sessions,
    };

    let app = Router::new().route("/", get(ws_handler)).with_state(state);
    let port = 8080;
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    println!("Game Server listening on {}", addr);

    axum::serve(listener, app).await.unwrap();
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

#[repr(u8)]
pub enum SocketType {
    HeartBeat,
    Ticket,
}

fn get_socket_type(value: u8) -> Option<SocketType> {
    match value {
        0 => Some(SocketType::HeartBeat),
        1 => Some(SocketType::Ticket),
        _ => None,
    }
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let (game_tx, mut game_rx) = mpsc::channel::<Message>(32);
    let mut connected_user_id: Option<String> = None;
    let mut redis_con = match state.redis_client.get_multiplexed_async_connection().await {
        Ok(con) => con,
        Err(e) => {
            println!("Redis connection failed: {}", e);
            return;
        }
    };
    println!("New WebSocket connection established!");

    loop {
        tokio::select! {
                Some(msg) = game_rx.recv() => {
                    if let Err(e) = sender.send(msg).await {
                        println!("Error sending message to client: {}", e);
                        break;
                    }
                }
                result = receiver.next() => {
                    match result {
                        Some(Ok(msg)) => {
                            match msg {
                                Message::Text(t) => {
                                    println!("Client sent: {}", t);
                                }
                                Message::Binary(bytes) => match get_socket_type(bytes[0]) {
                                    Some(SocketType::HeartBeat) => {
                                        println!("Received HeartBeat");
                                        let mut response = BytesMut::with_capacity(1);
                                        response.put_u8(0);
                                        if let Err(e) = sender.send(Message::Binary(response.to_vec())).await {
                                            println!("Error sending HeartBeat response: {}", e);
                                        }
                                    }
                                    Some(SocketType::Ticket) => {
                                        match String::from_utf8(bytes[1..].to_vec()) {
                                            Ok(ticket) => {
                                                println!("Received Ticket : {}", ticket);
                                                let rediskey = format!("game_ticket:{}", ticket);
                                                let result: redis::RedisResult<Option<String>> =
                                                    redis_con.get_del(&rediskey).await;
                                                match result {
                                                    Ok(Some(data)) => {
                                                        println!("Valid Ticket! User ID: {}", data);
                                                        state.sessions.insert(data.clone(), game_tx.clone());
                                                                    connected_user_id = Some(data);
                                                        _ = sender.send(Message::Text("우끼끼".to_string())).await;
                                                        // TODO
                                                    }
                                                    Ok(None) => {
                                                        println!("Invalid Ticket (Not Found)");
                                                        break;
                                                    }
                                                    Err(e) => println!("Redis Error: {}", e),
                                                }
                                            }
                                            Err(e) => {
                                                println!("Error decoding Ticket: {}", e);
                                            }
                                        }
                                    }
                                    None => {
                                        println!("Unknown SocketType received");
                                    }
                                },
                                Message::Close(_) => {
                                    println!("Client disconnected");
                                    break;
                                }
                                _ => {}
                            }
                        }
                        Some(Err(e)) => {
                            println!("WebSocket error: {}", e);
                            break;
                        },
                        None => {
                            println!("WebSocket connection closed");
                            break;
                        }
                    }
                }
        }
    }
    if let Some(user_id) = connected_user_id {
        println!("User Disconnected: {}", user_id);
        state.sessions.remove(&user_id);
    }
}
