
use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
    Router,
};
use std::net::SocketAddr;
use bytes::{BufMut, BytesMut};

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/", get(ws_handler));
    let port = 8080;
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    println!("Game Server listening on {}", addr);

    axum::serve(listener, app).await.unwrap();
}



async fn ws_handler(
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(handle_socket)
}

async fn handle_socket(mut socket: WebSocket) {
    println!("New WebSocket connection established!");
    while let Some(msg) = socket.recv().await {
        match msg {
            Ok(msg) => {
                match msg {
                    Message::Text(t) => {
                        println!("Client sent: {}", t);
                    }
                    Message::Binary(bytes) => {
                        match str::from_utf8(&bytes) {
                            Ok(text) => {
                                println!("Received Text (from Binary): {}", text);
                                if socket.send(Message::Text(format!("Server echo: {}", text))).await.is_err() {
                                    println!("Failed to send message");
                                    break;
                                }
                            },
                            Err(_) => {
                                let a= u8::from_le_bytes([bytes[0]]);
                                let b= u16::from_le_bytes([bytes[1], bytes[2]]);
                                let c= i16::from_le_bytes([bytes[3], bytes[4]]);
                                println!("Received Real Binary Data: {} {} {}", a, b, c);
                                let mut buf = BytesMut::with_capacity(128);
                                buf.put_u16_le(a.into());
                                buf.put_u16_le(b);
                                buf.put_i16_le(c);
                                if socket.send(Message::Binary(buf.to_vec())).await.is_err() {
                                    println!("Failed to send message");
                                    break;
                                }
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
            Err(e) => {
                println!("Error: {}", e);
                break;
            }
        }
    }
}