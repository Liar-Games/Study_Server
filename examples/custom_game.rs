//! 카운터 게임: 입장·재접속 때 현재 상태를 보내고 룸 입력으로 값을 변경한다.
use std::sync::Arc;
use study_server::{
    auth::GuestAuth,
    engine::{Config, Engine},
    game::{GameEvent, GameLogic, RoomContext},
    server,
};

#[derive(Default)]
struct Counter {
    value: u64,
}
impl GameLogic for Counter {
    fn on_event(&mut self, event: GameEvent, ctx: &mut RoomContext<'_>) {
        match event {
            GameEvent::Joined(player) | GameEvent::Reconnected(player) => {
                // 오프라인 출력은 저장하지 않으므로 현재 상태를 다시 전송한다.
                let _ = ctx.send(
                    player.session_id,
                    1,
                    self.value.to_be_bytes().to_vec().into(),
                );
            }
            GameEvent::Message {
                opcode: 1, payload, ..
            } if payload.is_empty() => {
                self.value = self.value.saturating_add(1);
                ctx.broadcast(1, self.value.to_be_bytes().to_vec().into());
            }
            GameEvent::Closing => {
                // 저장이 필요하면 미리 만든 bounded worker 큐에 try_send한다.
                // 콜백에서 I/O하거나 제한 없이 task를 생성하지 않는다.
            }
            _ => {}
        }
    }
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 기본 설정은 게임방 생성·참가·매칭을 허용하고 글로벌 룸 입력을 차단한다.
    let engine = Engine::new(Config::default(), |_| Box::new(Counter::default())).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
    let stop = engine.clone();
    axum::serve(
        listener,
        server::router(engine.clone(), Arc::new(GuestAuth)),
    )
    .tcp_nodelay(true)
    .with_graceful_shutdown(async move {
        let _ = tokio::signal::ctrl_c().await;
        stop.shutdown().await;
    })
    .await?;
    engine.shutdown().await;
    Ok(())
}
