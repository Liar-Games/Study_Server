use bytes::Bytes;
use std::time::Duration;
use study_server::{
    Error,
    actors::client::{ClientEvent, ClientInbox},
    engine::{Config, Engine},
    game::{GameEvent, GameLogic, RoomContext},
    protocol::GameFrame,
    types::Principal,
};

struct Echo;
impl GameLogic for Echo {
    fn on_event(&mut self, event: GameEvent, ctx: &mut RoomContext<'_>) {
        if let GameEvent::Message {
            player,
            opcode,
            payload,
        } = event
        {
            if opcode == 2 {
                ctx.broadcast_except(player, opcode, payload);
            } else {
                let _ = ctx.send(player, opcode, payload);
            }
        }
    }
}
async fn engine(mut config: Config) -> Engine {
    config.allow_global_input = true;
    Engine::new(config, |_| Box::new(Echo)).await.unwrap()
}
async fn game(inbox: &mut ClientInbox) -> GameFrame {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(ClientEvent::Frame(data)) = inbox.recv().await {
                if data[0] == 10 {
                    return GameFrame::decode(data).unwrap();
                }
            } else {
                panic!("closed");
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn distinct_guests_join_global_and_packets_stay_in_their_room() {
    let e = engine(Config::default()).await;
    let mut a = e.connect(Principal::guest(), "a".into()).await.unwrap();
    let mut b = e.connect(Principal::guest(), "b".into()).await.unwrap();
    assert_ne!(a.handle.player().principal, b.handle.player().principal);
    let global = a.inbox.next_room().await.unwrap();
    let _ = b.inbox.next_room().await.unwrap();
    assert_eq!(global.id(), 0);
    let id = e.create(&a.handle, 2).await.unwrap();
    let room = a.inbox.next_room().await.unwrap();
    room.packet(a.handle.id(), 1, Bytes::from_static(b"room-only"))
        .unwrap();
    assert_eq!(game(&mut a.inbox).await.room_id, id);
    assert_eq!(room.packet(b.handle.id(), 1, Bytes::new()), Ok(())); // room rejects this at consumption
    assert!(
        tokio::time::timeout(Duration::from_millis(30), b.inbox.recv())
            .await
            .is_err()
    );
    e.join(&b.handle, id).await.unwrap();
    b.inbox.next_room().await.unwrap();
    room.packet(a.handle.id(), 2, Bytes::from_static(b"other"))
        .unwrap();
    assert_eq!(game(&mut b.inbox).await.payload, b"other"[..]);
    e.disconnect(a.handle.id()).await;
    e.disconnect(b.handle.id()).await;
    assert_eq!(e.stats().await.unwrap().rooms, 1);
    assert_eq!(e.stats().await.unwrap().sessions, 0);
    e.shutdown().await;
}

#[tokio::test]
async fn duplicate_account_and_stale_disconnect_cannot_replace_new_session() {
    let e = engine(Config::default()).await;
    let principal = Principal::Account(uuid::Uuid::new_v4());
    let a = e.connect(principal.clone(), "old".into()).await.unwrap();
    assert!(matches!(
        e.connect(principal.clone(), "renamed".into()).await,
        Err(Error::AlreadyConnected)
    ));
    e.disconnect(a.handle.id()).await;
    let b = e.connect(principal, "new".into()).await.unwrap();
    e.disconnect(a.handle.id()).await;
    assert_eq!(e.stats().await.unwrap().sessions, 1);
    assert!(!b.handle.is_closed());
    e.shutdown().await;
}

#[tokio::test]
async fn queue_is_unique_cancellable_and_matches_distinct_live_players() {
    let e = engine(Config::default()).await;
    let mut a = e.connect(Principal::guest(), "a".into()).await.unwrap();
    let mut b = e.connect(Principal::guest(), "b".into()).await.unwrap();
    a.inbox.next_room().await.unwrap();
    b.inbox.next_room().await.unwrap();
    e.queue(&a.handle).await.unwrap();
    assert_eq!(e.queue(&a.handle).await, Err(Error::AlreadyQueued));
    e.cancel_queue(a.handle.id()).await.unwrap();
    assert_eq!(e.stats().await.unwrap().queued, 0);
    e.queue(&a.handle).await.unwrap();
    e.queue(&b.handle).await.unwrap();
    let ar = tokio::time::timeout(Duration::from_secs(1), a.inbox.next_room())
        .await
        .unwrap()
        .unwrap();
    let br = b.inbox.next_room().await.unwrap();
    assert_ne!(ar.id(), 0);
    assert_eq!(ar.id(), br.id());
    e.disconnect(a.handle.id()).await;
    e.disconnect(b.handle.id()).await;
    assert_eq!(e.stats().await.unwrap().rooms, 1);
    e.shutdown().await;
}

#[tokio::test]
async fn full_recipient_does_not_block_room_or_other_recipients() {
    let e = engine(Config {
        outbound_capacity: 4,
        ..Config::default()
    })
    .await;
    let mut a = e.connect(Principal::guest(), "slow".into()).await.unwrap();
    let mut b = e.connect(Principal::guest(), "fast".into()).await.unwrap();
    let global = a.inbox.next_room().await.unwrap();
    b.inbox.next_room().await.unwrap();
    for _ in 0..8 {
        global
            .packet(b.handle.id(), 2, Bytes::from_static(b"fill slow"))
            .unwrap();
        global
            .packet(b.handle.id(), 1, Bytes::from_static(b"fast"))
            .unwrap();
        assert_eq!(game(&mut b.inbox).await.payload, b"fast"[..]);
    }
    assert!(a.handle.is_closed());
    assert!(!b.handle.is_closed());
    e.shutdown().await;
}

#[tokio::test]
async fn capacity_and_membership_limits_do_not_leave_empty_rooms() {
    let e = engine(Config {
        max_room_players: 2,
        max_rooms: 3,
        ..Config::default()
    })
    .await;
    let a = e.connect(Principal::guest(), "a".into()).await.unwrap();
    let b = e.connect(Principal::guest(), "b".into()).await.unwrap();
    let c = e.connect(Principal::guest(), "c".into()).await.unwrap();
    assert_eq!(e.create(&a.handle, 0).await, Err(Error::Limit));
    let room = e.create(&a.handle, 2).await.unwrap();
    e.join(&b.handle, room).await.unwrap();
    assert_eq!(e.join(&c.handle, room).await, Err(Error::RoomFull));
    assert_eq!(e.create(&a.handle, 2).await, Err(Error::Limit));
    assert_eq!(e.stats().await.unwrap().rooms, 2);
    assert_eq!(e.leave(&a.handle, 0).await, Err(Error::Denied));
    e.leave(&a.handle, room).await.unwrap();
    e.leave(&b.handle, room).await.unwrap();
    assert_eq!(e.join(&c.handle, room).await, Err(Error::RoomNotFound));
    assert_eq!(e.stats().await.unwrap().rooms, 1);
    e.shutdown().await;
}

#[tokio::test]
async fn failed_global_admission_releases_account_registration() {
    struct Deny;
    impl GameLogic for Deny {
        fn admit(&self, _: &study_server::types::Player) -> bool {
            false
        }
        fn on_event(&mut self, _: GameEvent, _: &mut RoomContext<'_>) {}
    }
    let e = Engine::new(Config::default(), |_| Box::new(Deny))
        .await
        .unwrap();
    let principal = Principal::Account(uuid::Uuid::new_v4());
    assert!(matches!(
        e.connect(principal.clone(), "a".into()).await,
        Err(Error::Denied)
    ));
    assert!(matches!(
        e.connect(principal, "a".into()).await,
        Err(Error::Denied)
    ));
    assert_eq!(e.stats().await.unwrap().sessions, 0);
    e.shutdown().await;
}

#[tokio::test]
async fn room_panic_invalidates_old_handle_without_killing_other_rooms() {
    struct Crashing;
    impl GameLogic for Crashing {
        fn on_event(&mut self, event: GameEvent, _: &mut RoomContext<'_>) {
            if matches!(event, GameEvent::Message { .. }) {
                panic!("deliberate game crash");
            }
        }
    }
    let e = Engine::new(
        Config {
            allow_global_input: true,
            ..Config::default()
        },
        |id| {
            if id == 0 {
                Box::new(Echo)
            } else {
                Box::new(Crashing)
            }
        },
    )
    .await
    .unwrap();
    let mut a = e.connect(Principal::guest(), "a".into()).await.unwrap();
    let global = a.inbox.next_room().await.unwrap();
    e.create(&a.handle, 2).await.unwrap();
    let room = a.inbox.next_room().await.unwrap();
    room.packet(a.handle.id(), 1, Bytes::new()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while e.stats().await.unwrap().rooms != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(room.is_closed());
    global
        .packet(a.handle.id(), 1, Bytes::from_static(b"alive"))
        .unwrap();
    // Room-close control events may precede the global response.
    loop {
        if let Some(ClientEvent::Frame(data)) = a.inbox.recv().await
            && data[0] == 10
        {
            assert_eq!(GameFrame::decode(data).unwrap().payload, b"alive"[..]);
            break;
        }
    }
    e.shutdown().await;
}

#[tokio::test]
async fn factory_panic_fails_request_without_killing_manager() {
    let e = Engine::new(
        Config {
            allow_global_input: true,
            ..Config::default()
        },
        |id| {
            if id != 0 {
                panic!("factory failed");
            }
            Box::new(Echo)
        },
    )
    .await
    .unwrap();
    let a = e.connect(Principal::guest(), "a".into()).await.unwrap();
    assert!(e.create(&a.handle, 2).await.is_err());
    assert_eq!(e.stats().await.unwrap().rooms, 1);
    e.shutdown().await;
}

#[tokio::test]
async fn queued_disconnect_is_removed_and_shutdown_closes_every_client() {
    let e = engine(Config::default()).await;
    let mut a = e.connect(Principal::guest(), "a".into()).await.unwrap();
    e.queue(&a.handle).await.unwrap();
    e.disconnect(a.handle.id()).await;
    assert_eq!(e.stats().await.unwrap().queued, 0);
    assert!(a.inbox.recv().await.is_none());
    let b = e.connect(Principal::guest(), "b".into()).await.unwrap();
    e.shutdown().await;
    assert!(b.handle.is_closed());
    assert!(matches!(
        e.connect(Principal::guest(), "c".into()).await,
        Err(Error::Unavailable)
    ));
}

#[tokio::test]
async fn global_crash_marks_engine_unavailable_and_closes_sessions() {
    struct Crash;
    impl GameLogic for Crash {
        fn on_event(&mut self, event: GameEvent, _: &mut RoomContext<'_>) {
            if matches!(event, GameEvent::Message { .. }) {
                panic!("global failure");
            }
        }
    }
    let e = Engine::new(
        Config {
            allow_global_input: true,
            ..Config::default()
        },
        |_| Box::new(Crash),
    )
    .await
    .unwrap();
    let mut a = e.connect(Principal::guest(), "a".into()).await.unwrap();
    let global = a.inbox.next_room().await.unwrap();
    global.packet(a.handle.id(), 1, Bytes::new()).unwrap();
    tokio::time::timeout(Duration::from_millis(200), async {
        while e.is_running() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    e.shutdown().await;
    assert!(a.handle.is_closed());
}

#[tokio::test]
async fn game_worker_returns_through_room_mailbox_and_scoped_fanout_deduplicates() {
    struct Worker;
    impl GameLogic for Worker {
        fn on_event(&mut self, event: GameEvent, ctx: &mut RoomContext<'_>) {
            match event {
                GameEvent::Message { opcode: 7, .. } => {
                    let room = ctx.handle();
                    tokio::spawn(async move {
                        let _ = room.event(GameEvent::System {
                            opcode: 8,
                            payload: Bytes::from_static(b"worker"),
                        });
                    });
                }
                GameEvent::System { opcode, payload } => {
                    let ids: Vec<_> = ctx.players().map(|p| p.session_id).collect();
                    let mut repeated = ids.clone();
                    repeated.extend(ids);
                    repeated.push(uuid::Uuid::nil());
                    assert_eq!(ctx.send_many(repeated, opcode, payload), 2);
                }
                _ => {}
            }
        }
    }
    let e = Engine::new(
        Config {
            allow_global_input: true,
            ..Config::default()
        },
        |_| Box::new(Worker),
    )
    .await
    .unwrap();
    let mut a = e.connect(Principal::guest(), "a".into()).await.unwrap();
    let mut b = e.connect(Principal::guest(), "b".into()).await.unwrap();
    let global = a.inbox.next_room().await.unwrap();
    b.inbox.next_room().await.unwrap();
    global.packet(a.handle.id(), 7, Bytes::new()).unwrap();
    assert_eq!(game(&mut a.inbox).await.payload, b"worker"[..]);
    assert_eq!(game(&mut b.inbox).await.payload, b"worker"[..]);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), a.inbox.recv())
            .await
            .is_err()
    );
    e.shutdown().await;
}

#[tokio::test]
async fn failed_join_callback_retracts_published_route() {
    struct CrashOnJoin;
    impl GameLogic for CrashOnJoin {
        fn on_event(&mut self, event: GameEvent, _: &mut RoomContext<'_>) {
            if matches!(event, GameEvent::Joined(_)) {
                panic!("join initialization failed");
            }
        }
    }
    let e = Engine::new(
        Config {
            allow_global_input: true,
            ..Config::default()
        },
        |id| {
            if id == 0 {
                Box::new(Echo)
            } else {
                Box::new(CrashOnJoin)
            }
        },
    )
    .await
    .unwrap();
    let mut a = e.connect(Principal::guest(), "a".into()).await.unwrap();
    a.inbox.next_room().await.unwrap();
    for _ in 0..3 {
        assert!(e.create(&a.handle, 2).await.is_err());
        let published = a.inbox.next_room().await.unwrap();
        let event = tokio::time::timeout(Duration::from_millis(100), a.inbox.recv())
            .await
            .unwrap();
        assert!(matches!(event, Some(ClientEvent::Left(id)) if id == published.id()));
    }
    assert_eq!(e.stats().await.unwrap().rooms, 1);
    assert!(!a.handle.is_closed());
    e.shutdown().await;
}

#[tokio::test]
async fn matching_admission_failure_rolls_back_every_participant() {
    struct Restricted;
    impl GameLogic for Restricted {
        fn admit(&self, player: &study_server::types::Player) -> bool {
            player.name != "denied"
        }
        fn on_event(&mut self, _: GameEvent, _: &mut RoomContext<'_>) {}
    }
    let e = Engine::new(
        Config {
            allow_global_input: true,
            ..Config::default()
        },
        |id| {
            if id == 0 {
                Box::new(Echo)
            } else {
                Box::new(Restricted)
            }
        },
    )
    .await
    .unwrap();
    let mut a = e
        .connect(Principal::guest(), "allowed".into())
        .await
        .unwrap();
    let b = e
        .connect(Principal::guest(), "denied".into())
        .await
        .unwrap();
    a.inbox.next_room().await.unwrap();
    e.queue(&a.handle).await.unwrap();
    e.queue(&b.handle).await.unwrap();
    let mut joined = None;
    let mut left = None;
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match a.inbox.recv().await.unwrap() {
                ClientEvent::Joined(room) => joined = Some(room.id()),
                ClientEvent::Left(room) => left = Some(room),
                ClientEvent::Frame(data) if data[0] == 4 => {
                    let value: serde_json::Value = serde_json::from_slice(&data[1..]).unwrap();
                    assert_eq!(value["error"], "denied");
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(joined.is_some());
    assert_eq!(joined, left);
    assert_eq!(e.stats().await.unwrap().rooms, 1);
    assert_eq!(e.stats().await.unwrap().queued, 0);
    e.shutdown().await;
}

#[tokio::test]
async fn trusted_m_to_n_resolves_unique_live_sessions_and_delivers_shared_frame() {
    let e = engine(Config::default()).await;
    let mut a = e.connect(Principal::guest(), "a".into()).await.unwrap();
    let mut b = e.connect(Principal::guest(), "b".into()).await.unwrap();
    a.inbox.next_room().await.unwrap();
    b.inbox.next_room().await.unwrap();
    let recipients = e
        .sessions()
        .resolve(vec![
            a.handle.id(),
            b.handle.id(),
            a.handle.id(),
            uuid::Uuid::nil(),
        ])
        .await
        .unwrap();
    assert_eq!(recipients.len(), 2);
    let frame = GameFrame {
        room_id: 0,
        opcode: 90,
        payload: Bytes::from_static(b"server"),
    }
    .encode();
    for client in recipients {
        client.send(frame.clone()).unwrap();
    }
    assert_eq!(game(&mut a.inbox).await.opcode, 90);
    assert_eq!(game(&mut b.inbox).await.opcode, 90);
    e.disconnect(a.handle.id()).await;
    assert!(
        e.sessions()
            .resolve(vec![a.handle.id()])
            .await
            .unwrap()
            .is_empty()
    );
    e.shutdown().await;
}

#[tokio::test]
async fn tick_can_end_a_room_and_releases_its_task() {
    struct Timed;
    impl GameLogic for Timed {
        fn tick_interval(&self) -> Option<Duration> {
            Some(Duration::from_millis(10))
        }
        fn on_event(&mut self, event: GameEvent, ctx: &mut RoomContext<'_>) {
            if matches!(event, GameEvent::Tick) {
                ctx.close();
            }
        }
    }
    let e = Engine::new(
        Config {
            allow_global_input: true,
            ..Config::default()
        },
        |id| {
            if id == 0 {
                Box::new(Echo)
            } else {
                Box::new(Timed)
            }
        },
    )
    .await
    .unwrap();
    let mut a = e.connect(Principal::guest(), "a".into()).await.unwrap();
    a.inbox.next_room().await.unwrap();
    let id = e.create(&a.handle, 2).await.unwrap();
    let room = a.inbox.next_room().await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let Some(ClientEvent::Left(room)) = a.inbox.recv().await {
                assert_eq!(room, id);
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(room.is_closed());
    assert_eq!(e.stats().await.unwrap().rooms, 1);
    e.shutdown().await;
}

#[tokio::test]
async fn json_helpers_deliver_room_scoped_payloads_and_report_serialization_failure() {
    struct CannotSerialize;
    impl serde::Serialize for CannotSerialize {
        fn serialize<S: serde::Serializer>(&self, _: S) -> std::result::Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("test failure"))
        }
    }
    struct JsonGame;
    impl GameLogic for JsonGame {
        fn on_event(&mut self, event: GameEvent, ctx: &mut RoomContext<'_>) {
            if let GameEvent::Message { player, .. } = event {
                assert_eq!(
                    ctx.send_json(player, 98, &CannotSerialize),
                    Err(Error::InvalidPacket)
                );
                assert_eq!(
                    ctx.broadcast_json(99, &CannotSerialize),
                    Err(Error::InvalidPacket)
                );
                assert_eq!(
                    ctx.broadcast_json(
                        40,
                        &serde_json::json!({"kind":"room", "players":ctx.players().count()})
                    ),
                    Ok(2)
                );
                ctx.send_json(
                    player,
                    41,
                    &serde_json::json!({"kind":"private", "value":7}),
                )
                .unwrap();
            }
        }
    }
    let e = Engine::new(Config::default(), |_| Box::new(JsonGame))
        .await
        .unwrap();
    let mut a = e.connect(Principal::guest(), "a".into()).await.unwrap();
    let mut b = e.connect(Principal::guest(), "b".into()).await.unwrap();
    let mut outsider = e
        .connect(Principal::guest(), "outside".into())
        .await
        .unwrap();
    a.inbox.next_room().await.unwrap();
    b.inbox.next_room().await.unwrap();
    outsider.inbox.next_room().await.unwrap();
    let id = e.create(&a.handle, 2).await.unwrap();
    let room = a.inbox.next_room().await.unwrap();
    e.join(&b.handle, id).await.unwrap();
    b.inbox.next_room().await.unwrap();
    room.packet(a.handle.id(), 1, Bytes::new()).unwrap();
    let broadcast = game(&mut a.inbox).await;
    assert_eq!(broadcast.opcode, 40);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&broadcast.payload).unwrap(),
        serde_json::json!({"kind":"room","players":2})
    );
    assert_eq!(game(&mut b.inbox).await.payload, broadcast.payload);
    let private = game(&mut a.inbox).await;
    assert_eq!(private.opcode, 41);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&private.payload).unwrap(),
        serde_json::json!({"kind":"private","value":7})
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(20), outsider.inbox.recv())
            .await
            .is_err()
    );
    e.shutdown().await;
}
