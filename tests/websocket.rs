use futures::{SinkExt, StreamExt};
use std::{sync::Arc, time::Duration};
use study_server::{
    auth::GuestAuth,
    engine::{Config, Engine},
    game::EchoGame,
    server,
};
use tokio_tungstenite::{connect_async, tungstenite::Message};

async fn start(mut config: Config) -> (Engine, String, tokio::task::JoinHandle<()>) {
    config.auth_timeout = Duration::from_millis(100);
    // Existing cleanup checks exercise final expiry without waiting the production 60s.
    config.reconnect_grace = Duration::from_millis(20);
    let engine = Engine::new(config, |_| Box::new(EchoGame)).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let app = server::router(engine.clone(), Arc::new(GuestAuth));
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (engine, url, task)
}
async fn recv<S>(ws: &mut tokio_tungstenite::WebSocketStream<S>) -> Vec<u8>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    match tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
    {
        Message::Binary(data) => data,
        other => panic!("unexpected {other:?}"),
    }
}
#[tokio::test]
async fn real_websocket_auth_create_route_and_cleanup() {
    let (e, url, task) = start(Config::default()).await;
    let (mut ws, _) = connect_async(url).await.unwrap();
    ws.send(Message::Binary(
        [vec![1], br#"{"mode":"guest","version":1}"#.to_vec()].concat(),
    ))
    .await
    .unwrap();
    let welcome = recv(&mut ws).await;
    assert_eq!(welcome[0], 2);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&welcome[1..]).unwrap()["global_room"],
        0
    );
    ws.send(Message::Binary(
        [
            vec![3],
            br#"{"request_id":7,"op":"create","capacity":2}"#.to_vec(),
        ]
        .concat(),
    ))
    .await
    .unwrap();
    let reply = recv(&mut ws).await;
    assert_eq!(reply[0], 4);
    let reply: serde_json::Value = serde_json::from_slice(&reply[1..]).unwrap();
    assert_eq!(reply["request_id"], 7);
    assert!(reply["error"].is_null(), "{reply}");
    let room = reply["room_id"].as_u64().unwrap();
    let mut packet = vec![10];
    packet.extend(room.to_be_bytes());
    packet.extend([0, 1, 42, 43]);
    ws.send(Message::Binary(packet.clone())).await.unwrap();
    assert_eq!(recv(&mut ws).await, packet);
    // Another room has not been admitted and must never receive this input.
    let mut bad = vec![10];
    bad.extend(999u64.to_be_bytes());
    bad.extend([0, 1]);
    ws.send(Message::Binary(bad)).await.unwrap();
    assert_eq!(recv(&mut ws).await[0], 255);
    ws.close(None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while e.stats().await.unwrap().sessions != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(e.stats().await.unwrap().rooms, 1);
    e.shutdown().await;
    task.abort();
}
#[tokio::test]
async fn authentication_deadline_and_invalid_ticket_do_not_register_guests() {
    let (e, url, task) = start(Config::default()).await;
    let (mut idle, _) = connect_async(&url).await.unwrap();
    let packet = recv(&mut idle).await;
    assert_eq!(packet[0], 255);
    let (mut invalid, _) = connect_async(&url).await.unwrap();
    invalid
        .send(Message::Binary(
            [
                vec![1],
                br#"{"mode":"ticket","version":1,"ticket":"invalid"}"#.to_vec(),
            ]
            .concat(),
        ))
        .await
        .unwrap();
    assert_eq!(recv(&mut invalid).await[0], 255);
    assert_eq!(e.stats().await.unwrap().sessions, 0);
    e.shutdown().await;
    task.abort();
}

#[tokio::test]
async fn pending_auth_connections_are_bounded_before_registration() {
    let (e, url, task) = start(Config {
        max_sessions: 1,
        ..Config::default()
    })
    .await;
    let (_pending, _) = connect_async(&url).await.unwrap();
    let second = connect_async(&url).await;
    assert!(
        matches!(second, Err(tokio_tungstenite::tungstenite::Error::Http(ref response)) if response.status() == 503)
    );
    e.shutdown().await;
    task.abort();
}

#[tokio::test]
async fn browser_origin_must_match_explicit_allowlist() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let (e, url, task) = start(Config {
        allowed_origins: vec!["https://game.test".into()],
        ..Config::default()
    })
    .await;
    assert!(connect_async(&url).await.is_err());
    let mut denied = url.clone().into_client_request().unwrap();
    denied
        .headers_mut()
        .insert("origin", "https://evil.test".parse().unwrap());
    assert!(connect_async(denied).await.is_err());
    let mut allowed = url.into_client_request().unwrap();
    allowed
        .headers_mut()
        .insert("origin", "https://game.test".parse().unwrap());
    let (mut ws, _) = connect_async(allowed).await.unwrap();
    ws.send(Message::Binary(
        [vec![1], br#"{"mode":"guest","version":1}"#.to_vec()].concat(),
    ))
    .await
    .unwrap();
    assert_eq!(recv(&mut ws).await[0], 2);
    e.shutdown().await;
    task.abort();
}

#[tokio::test]
async fn oversized_and_rate_limited_clients_release_their_sessions() {
    for config in [
        Config {
            max_frame: 512,
            ..Config::default()
        },
        Config {
            messages_per_second: 2,
            ..Config::default()
        },
    ] {
        let oversized = config.max_frame == 512;
        let (e, url, task) = start(config).await;
        let (mut ws, _) = connect_async(&url).await.unwrap();
        ws.send(Message::Binary(
            [vec![1], br#"{"mode":"guest","version":1}"#.to_vec()].concat(),
        ))
        .await
        .unwrap();
        recv(&mut ws).await;
        if oversized {
            ws.send(Message::Binary(vec![10; 513])).await.unwrap();
        } else {
            for _ in 0..3 {
                ws.send(Message::Ping(vec![])).await.unwrap();
            }
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while e.stats().await.unwrap().sessions != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        e.shutdown().await;
        task.abort();
    }
}

#[tokio::test]
async fn two_real_clients_match_and_exchange_room_packets() {
    let (e, url, task) = start(Config::default()).await;
    let (mut a, _) = connect_async(&url).await.unwrap();
    let (mut b, _) = connect_async(&url).await.unwrap();
    let auth = [vec![1], br#"{"mode":"guest","version":1}"#.to_vec()].concat();
    a.send(Message::Binary(auth.clone())).await.unwrap();
    b.send(Message::Binary(auth)).await.unwrap();
    recv(&mut a).await;
    recv(&mut b).await;
    let queue = [vec![3], br#"{"request_id":9,"op":"queue"}"#.to_vec()].concat();
    a.send(Message::Binary(queue.clone())).await.unwrap();
    let ack = recv(&mut a).await;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&ack[1..]).unwrap()["request_id"],
        9
    );
    b.send(Message::Binary(queue)).await.unwrap();
    let matched = recv(&mut a).await;
    let room = serde_json::from_slice::<serde_json::Value>(&matched[1..]).unwrap()["room_id"]
        .as_u64()
        .unwrap();
    let mut b_matched = false;
    for _ in 0..2 {
        let data = recv(&mut b).await;
        let result: serde_json::Value = serde_json::from_slice(&data[1..]).unwrap();
        assert!(result["error"].is_null());
        if result["request_id"].is_null() {
            assert_eq!(result["room_id"], room);
            b_matched = true;
        }
    }
    assert!(b_matched);
    let mut packet = vec![10];
    packet.extend(room.to_be_bytes());
    packet.extend([0, 2, 17]);
    a.send(Message::Binary(packet.clone())).await.unwrap();
    assert_eq!(recv(&mut b).await, packet);
    a.close(None).await.unwrap();
    b.close(None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while e.stats().await.unwrap().sessions != 0 || e.stats().await.unwrap().rooms != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    e.shutdown().await;
    task.abort();
}

#[tokio::test]
async fn burst_output_preserves_all_packet_boundaries_and_order() {
    let (e, url, task) = start(Config {
        allow_global_input: true,
        ..Config::default()
    })
    .await;
    let (mut ws, _) = connect_async(&url).await.unwrap();
    ws.send(Message::Binary(
        [vec![1], br#"{"mode":"guest","version":1}"#.to_vec()].concat(),
    ))
    .await
    .unwrap();
    recv(&mut ws).await;
    for n in 0u32..100 {
        let mut packet = vec![10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        packet.extend(n.to_be_bytes());
        ws.feed(Message::Binary(packet)).await.unwrap();
    }
    ws.flush().await.unwrap();
    for n in 0u32..100 {
        let reply = recv(&mut ws).await;
        assert_eq!(&reply[..11], &[10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        assert_eq!(&reply[11..], &n.to_be_bytes());
    }
    e.shutdown().await;
    task.abort();
}
