use std::{env, sync::Arc, time::Duration};
use study_server::{
    auth::GuestAuth,
    engine::{Config, Engine},
    game::EchoGame,
    server,
};
fn number<T: std::str::FromStr>(key: &str, default: T) -> Result<T, Box<dyn std::error::Error>> {
    match env::var(key) {
        Ok(value) => value.parse().map_err(|_| format!("invalid {key}").into()),
        Err(_) => Ok(default),
    }
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !number("ALLOW_GUESTS", true)? {
        return Err(
            "this example requires ALLOW_GUESTS=true; inject an Authenticator in your application"
                .into(),
        );
    }
    let mut config = Config::default();
    config.max_sessions = number("MAX_SESSIONS", config.max_sessions)?;
    config.max_rooms = number("MAX_ROOMS", config.max_rooms)?;
    config.max_room_players = number("MAX_ROOM_PLAYERS", config.max_room_players)?;
    config.max_rooms_per_session = number("MAX_ROOMS_PER_SESSION", config.max_rooms_per_session)?;
    config.outbound_capacity = number("OUTBOUND_CAPACITY", config.outbound_capacity)?;
    config.room_capacity = number("ROOM_MAILBOX_CAPACITY", config.room_capacity)?;
    config.max_frame = number("MAX_FRAME_BYTES", config.max_frame)?;
    config.match_size = number("MATCH_SIZE", config.match_size)?;
    config.messages_per_second = number("MESSAGES_PER_SECOND", config.messages_per_second)?;
    config.ping_interval = Duration::from_secs(number("PING_INTERVAL_SECONDS", 20)?);
    config.pong_timeout = Duration::from_secs(number("PONG_TIMEOUT_SECONDS", 10)?);
    config.reconnect_grace = Duration::from_secs(number("RECONNECT_GRACE_SECONDS", 60)?);
    config.allow_create = number("ALLOW_CREATE", true)?;
    config.allow_join = number("ALLOW_JOIN", true)?;
    config.allow_queue = number("ALLOW_QUEUE", true)?;
    config.allow_global_input = number("ALLOW_GLOBAL_INPUT", false)?;
    config.allowed_origins = env::var("ALLOWED_ORIGINS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let engine = Engine::new(config, |_| Box::new(EchoGame)).await?;
    let listener = tokio::net::TcpListener::bind((
        env::var("HOST")
            .unwrap_or_else(|_| "0.0.0.0".into())
            .as_str(),
        number("PORT", 8080u16)?,
    ))
    .await?;
    println!("study_server listening on {}", listener.local_addr()?);
    let stop = engine.clone();
    axum::serve(
        listener,
        server::router(engine.clone(), Arc::new(GuestAuth)),
    )
    .tcp_nodelay(true)
    .with_graceful_shutdown(async move {
        #[cfg(unix)]
        {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("SIGTERM handler");
            tokio::select! {_=term.recv()=>{},_=tokio::signal::ctrl_c()=>{}}
        }
        #[cfg(not(unix))]
        let _ = tokio::signal::ctrl_c().await;
        stop.shutdown().await;
    })
    .await?;
    engine.shutdown().await;
    Ok(())
}
