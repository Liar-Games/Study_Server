use axum::{
    Router,
    extract::{State, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
};
use std::env;
use std::net::SocketAddr;

/* Custom Libraries */
use study_server::{AppError, Result}; /* Error Handling */
mod actors;
use actors::client::ClientActor;
use actors::session_manager::{SessionManager,SessionHandle};
use actors::room_manager::{RoomManager, RoomManagerHandle};

// Shared resources
#[derive(Clone)]
pub struct AppState {
    // TODO (Later) : use connection pool
    pub redis_client: redis::Client,
    pub session_handle: SessionHandle,
    pub room_manager: RoomManagerHandle,
}

#[tokio::main]
async fn main() -> Result<()> {
    // redis client
    // TODO (Later) : use connection pool
    let redis_url = env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1/".to_string());
    let redis = redis::Client::open(redis_url).map_err(|e| AppError::Config {
        reason: format!("Invalid Redis URL: {e}").into(),
    })?;

    // [Actor] Start Session Manager
    let (manager, handle) = SessionManager::new();
    tokio::spawn(async move {
        println!("Session Manager Started");
        manager.run().await;
    });

    // [Actor] Start Room Manager
    // Note : RoomManger start "global" room actor too
    let (room_manager_actor, room_manager) = RoomManager::new();
    tokio::spawn(async move {
        println!("Room Manager Started");
        room_manager_actor.run().await;
    });

    /////////
    // you may add some rooms here if needed
    /////////

    // Application State
    let state = AppState {
        redis_client: redis,
        session_handle: handle,
        room_manager: room_manager,
    };

    // server setup
    // TODO : hardcoded enviorment variables
    let app = Router::new().route("/", get(ws_handler)).with_state(state);
    let port = 8080;
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| AppError::Bind {
            addr: addr.to_string(),
            source: e,
        })?;
    println!("Game Server listening on {}", addr);

    // TODO : graceful shutdown
    // save every room's data to DB before shutdown
    axum::serve(listener, app)
        .await
        .map_err(|e| AppError::Serve { source: e })?;
    Ok(())
}

// websocket handler for axum
// creates ClientActor and run for each connection
async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| {
        async move {
            // create ClientActor and start it
            ClientActor::start(
                socket,
                state.session_handle,
                state.redis_client,
                state.room_manager,
            )
            .await;
        }
    })
}
