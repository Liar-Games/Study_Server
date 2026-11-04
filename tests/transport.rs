use futures::{SinkExt, StreamExt};
use std::{sync::Arc, time::Duration};
use study_server::{
    auth::GuestAuth,
    engine::{Config, Engine},
    game::EchoGame,
    server,
};
use tokio_tungstenite::{connect_async, tungstenite::Message};

async fn start(config: Config) -> (Engine, String, tokio::task::JoinHandle<()>) {
    let engine = Engine::new(config, |_| Box::new(EchoGame)).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let app = server::router(engine.clone(), Arc::new(GuestAuth));
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (engine, url, task)
}

#[tokio::test]
async fn global_client_game_input_is_denied_by_default() {
    let (engine, url, task) = start(Config::default()).await;
    let (mut ws, _) = connect_async(url).await.unwrap();
    ws.send(Message::Binary(
        [vec![1], br#"{"mode":"guest","version":1}"#.to_vec()].concat(),
    ))
    .await
    .unwrap();
    let welcome = ws.next().await.unwrap().unwrap();
    assert!(matches!(welcome, Message::Binary(_)));
    ws.send(Message::Binary(vec![10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]))
        .await
        .unwrap();
    let message = tokio::time::timeout(Duration::from_secs(1), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Message::Binary(data) = message else {
        panic!("expected error frame")
    };
    assert_eq!(data[0], 255);
    assert_eq!(&data[1..], br#""denied""#);
    engine.shutdown().await;
    task.abort();
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
async fn binary(ws: &mut Socket) -> Vec<u8> {
    loop {
        match tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
        {
            Message::Binary(frame) => return frame,
            Message::Ping(data) => ws.send(Message::Pong(data)).await.unwrap(),
            other => panic!("unexpected {other:?}"),
        }
    }
}
async fn auth(ws: &mut Socket, value: serde_json::Value) -> serde_json::Value {
    ws.send(Message::Binary(
        study_server::protocol::json(1, &value).to_vec(),
    ))
    .await
    .unwrap();
    let frame = binary(ws).await;
    assert_eq!(frame[0], 2);
    serde_json::from_slice(&frame[1..]).unwrap()
}
#[tokio::test]
async fn passive_clients_stay_connected_with_pong_and_wrong_pongs_do_not_keep_them_alive() {
    let (engine, url, task) = start(Config {
        ping_interval: Duration::from_millis(30),
        pong_timeout: Duration::from_millis(30),
        ..Config::default()
    })
    .await;
    let (mut ws, _) = connect_async(url).await.unwrap();
    let welcome = auth(&mut ws, serde_json::json!({"mode":"guest","version":1})).await;
    let id = serde_json::from_value(welcome["player"]["session_id"].clone()).unwrap();
    for _ in 0..3 {
        let Message::Ping(data) = tokio::time::timeout(Duration::from_secs(1), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
        else {
            panic!("missing ping")
        };
        ws.send(Message::Pong(data)).await.unwrap();
    }
    let handle = engine
        .sessions()
        .resolve(vec![id])
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert!(handle.is_current(1));
    let Message::Ping(_) = ws.next().await.unwrap().unwrap() else {
        panic!("missing next ping")
    };
    ws.send(Message::Pong(vec![9, 9])).await.unwrap();
    // Stop reading: a standards-compliant client would otherwise auto-answer Ping.
    tokio::time::timeout(Duration::from_secs(1), async {
        while handle.is_current(1) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(engine.stats().await.unwrap().sessions, 1); // suspended, not deleted
    engine.shutdown().await;
    task.abort();
}
#[tokio::test]
async fn real_socket_resume_restores_routing_and_old_socket_close_cannot_remove_it() {
    let (engine, url, task) = start(Config {
        max_sessions: 1,
        ..Config::default()
    })
    .await;
    let (mut old, _) = connect_async(&url).await.unwrap();
    let first = auth(&mut old, serde_json::json!({"mode":"guest","version":1})).await;
    old.send(Message::Binary(
        study_server::protocol::json(
            3,
            &serde_json::json!({"request_id":1,"op":"create","capacity":2}),
        )
        .to_vec(),
    ))
    .await
    .unwrap();
    let created = binary(&mut old).await;
    let reply: serde_json::Value = serde_json::from_slice(&created[1..]).unwrap();
    let room = reply["room_id"].as_u64().unwrap();
    let (mut resumed, _) = connect_async(&url).await.unwrap();
    let second=auth(&mut resumed,serde_json::json!({"mode":"resume","version":1,"session_id":first["player"]["session_id"],"token":first["resume_token"]})).await;
    assert_eq!(
        first["player"]["session_id"],
        second["player"]["session_id"]
    );
    assert_eq!(second["resumed"], true);
    assert_ne!(first["resume_token"], second["resume_token"]);
    let _ = old.close(None).await;
    let frame = study_server::protocol::GameFrame {
        room_id: room,
        opcode: 1,
        payload: bytes::Bytes::from_static(b"resumed"),
    }
    .encode();
    resumed.send(Message::Binary(frame.to_vec())).await.unwrap();
    assert_eq!(binary(&mut resumed).await, frame);
    assert_eq!(engine.stats().await.unwrap().sessions, 1);
    engine.shutdown().await;
    task.abort();
}
