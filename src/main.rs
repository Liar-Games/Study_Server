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

/* Custom Libraries */
use study_server::{AppError, Result}; /* Error Handling */

type SessionMap = Arc<DashMap<String, mpsc::Sender<Message>>>;

#[derive(Clone)]
pub struct AppState {
    pub redis_client: redis::Client,
    pub sessions: SessionMap,
}

#[tokio::main]
async fn main() -> Result<()> {
    let redis_url = env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1/".to_string());
    let redis = redis::Client::open(redis_url).map_err(|e| AppError::Config {
        reason: format!("Invalid Redis URL: {e}").into()
    })?;
    let sessions = Arc::new(DashMap::new());
    let state = AppState {
        redis_client: redis,
        sessions,
    };

    let app = Router::new().route("/", get(ws_handler)).with_state(state);
    let port = 8080;
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| AppError::Bind {
        addr: addr.to_string(),
        source: e,
    })?;
    println!("Game Server listening on {}", addr);

    axum::serve(listener, app).await.map_err(|e| AppError::Serve { source: e })?;
    Ok(())
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| {
        async move {
            if let Err(e) = handle_socket(socket, state).await {
                eprintln!("handle_socket error: {e}");
            }
        }
    })
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

/* Errors due to the client's responsibility (protocol/authentication, etc.) are normally terminated after sending an error message without killing the task and sending a close. */
async fn send_error_and_close(sender: &mut futures::stream::SplitSink<WebSocket, Message>, err: &AppError) {
    let (code, msg) = err.to_client_message();

    /* Send error message */
    let _ = sender.send(Message::Text(format!("{code}:{msg}"))).await;

    /* Send close */
    let _ = sender.send(Message::Close(None)).await;
}

async fn handle_socket(socket: WebSocket, state: AppState) -> Result<()> {
    let (mut sender, mut receiver) = socket.split();
    let (game_tx, mut game_rx) = mpsc::channel::<Message>(32);
    let mut connected_user_id: Option<String> = None;
    let mut redis_con = state.redis_client.get_multiplexed_async_connection().await.map_err(|e| AppError::Redis {
        reason: "failed to create multiplexed redis connection".into(),
        source: e,
    })?;
    println!("New WebSocket connection established!");

    loop {
        tokio::select! {
            Some(msg) = game_rx.recv() => {
                sender.send(msg).await.map_err(|e| AppError::WebSocket {
                    reason: format!("ws send failed: {e}").into(),
                })?;
            }

            result = receiver.next() => {
                match result {
                    Some(Ok(msg)) => {
                        match msg {
                            Message::Text(t) => {
                                println!("Client sent: {}", t);
                            }

                            Message::Binary(bytes) => {
                                if bytes.is_empty() {
                                    let err = AppError::InvalidFrame { reason: "binary payload is empty".into() };
                                    send_error_and_close(&mut sender, &err).await;
                                    break;
                                }

                                match get_socket_type(bytes[0]) {
                                    Some(SocketType::HeartBeat) => {
                                        println!("Received HeartBeat");

                                        let mut response = BytesMut::with_capacity(1);
                                        response.put_u8(0);

                                        sender.send(Message::Binary(response.to_vec())).await.map_err(|e| AppError::WebSocket {
                                            reason: format!("ws send failed: {e}").into(),
                                        })?;
                                    }

                                    Some(SocketType::Ticket) => {
                                        if bytes.len() < 2 {
                                            let err = AppError::InvalidFrame { reason: "ticket frame missing payload".into() };
                                            send_error_and_close(&mut sender, &err).await;
                                            break;
                                        }

                                        let ticket = String::from_utf8(bytes[1..].to_vec())
                                            .map_err(|e| AppError::Utf8 {
                                                field: "ticket".into(),
                                                source: e,
                                            })?;

                                        if ticket.is_empty() {
                                            let err = AppError::InvalidFrame { reason: "ticket is empty".into() };
                                            send_error_and_close(&mut sender, &err).await;
                                            break;
                                        }

                                        println!("Received Ticket : {}", ticket);

                                        let rediskey = format!("game_ticket:{}", ticket);

                                        let result: Option<String> = redis_con
                                            .get_del(&rediskey)
                                            .await
                                            .map_err(|e| AppError::TicketLookupFailed {
                                                key: rediskey.clone(),
                                                source: e,
                                            })?;

                                        match result {
                                            Some(user_id) => {
                                                println!("Valid Ticket! User ID: {}", user_id);

                                                if state.sessions.contains_key(&user_id) {
                                                    let err = AppError::DuplicateLogin { user_id };
                                                    send_error_and_close(&mut sender, &err).await;
                                                    break;
                                                }

                                                state.sessions.insert(user_id.clone(), game_tx.clone());
                                                connected_user_id = Some(user_id);

                                                sender.send(Message::Text("우끼끼".to_string())).await.map_err(|e| AppError::WebSocket {
                                                    reason: format!("ws send failed: {e}").into(),
                                                })?;
                                            }
                                            None => {
                                                let err = AppError::InvalidTicket;
                                                send_error_and_close(&mut sender, &err).await;
                                                break;
                                            }
                                        }
                                    }
                                    
                                    None => {
                                        let err = AppError::UnknownSocketType { value: bytes[0] };
                                        send_error_and_close(&mut sender, &err).await;
                                        break;
                                    }
                                }
                            }

                            Message::Close(_) => {
                                println!("Client disconnected");
                                break;
                            }

                            _ => {}
                        }
                    }

                    Some(Err(e)) => {
                        return Err(AppError::WebSocket {
                            reason: format!("websocket receive error: {e}").into(),
                        });
                    }

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

    Ok(())
}
