use bytes::Bytes;
use std::time::Duration;
use study_server::{
    Error,
    actors::client::ClientEvent,
    engine::{Config, Engine},
    game::{EchoGame, GameEvent, GameLogic, RoomContext},
    types::{Identity, Principal},
};

#[tokio::test]
async fn occupied_player_cannot_consume_a_valid_match_waiter() {
    let e = Engine::new(Config::default(), |_| Box::new(EchoGame))
        .await
        .unwrap();
    let a = e
        .connect(Principal::guest(), "occupied".into())
        .await
        .unwrap();
    let b = e
        .connect(Principal::guest(), "waiting".into())
        .await
        .unwrap();
    e.create(&a.handle, 2).await.unwrap();
    e.queue(&b.handle).await.unwrap();
    assert_eq!(e.queue(&a.handle).await, Err(Error::Denied));
    assert_eq!(e.stats().await.unwrap().queued, 1);
    e.shutdown().await;
}

#[derive(Default)]
struct Lifecycle {
    messages: u64,
}
impl GameLogic for Lifecycle {
    fn on_event(&mut self, event: GameEvent, ctx: &mut RoomContext<'_>) {
        match event {
            GameEvent::Reconnected(player) if ctx.room_id() != 0 => {
                assert_eq!(ctx.host(), Some(player.session_id));
                let _ = ctx.send(
                    player.session_id,
                    77,
                    Bytes::copy_from_slice(&self.messages.to_be_bytes()),
                );
            }
            GameEvent::Message {
                player,
                opcode,
                payload,
            } => {
                self.messages += 1;
                let _ = ctx.send(player, opcode, payload);
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn guest_resume_retains_room_rotates_proof_and_ignores_old_generation() {
    let e = Engine::new(
        Config {
            reconnect_grace: Duration::from_millis(80),
            ..Config::default()
        },
        |_| Box::new(Lifecycle::default()),
    )
    .await
    .unwrap();
    let mut a = e.connect(Principal::guest(), "guest".into()).await.unwrap();
    a.inbox.next_room().await.unwrap();
    let room_id = e.create(&a.handle, 2).await.unwrap();
    let room = a.inbox.next_room().await.unwrap();
    let id = a.handle.id();
    room.packet(id, 1, Bytes::from_static(b"before disconnect"))
        .unwrap();
    assert!(matches!(a.inbox.recv().await, Some(ClientEvent::Frame(_))));
    e.suspend(id, a.generation).await;
    assert_eq!(
        a.handle.send(Bytes::from_static(b"discard")),
        Err(Error::Closed)
    );
    assert_eq!(e.stats().await.unwrap().sessions, 1);
    assert!(matches!(
        e.resume(id, &id.to_string(), None).await,
        Err(Error::AuthFailed)
    ));
    let mut resumed = e.resume(id, &a.resume_token, None).await.unwrap();
    assert!(resumed.resumed);
    assert_ne!(a.resume_token, resumed.resume_token);
    assert_eq!(resumed.handle.id(), id);
    assert!(a.handle.is_current(resumed.generation));
    assert!(!a.handle.is_current(a.generation));
    // Every route is published before callbacks can enqueue snapshots.
    assert_eq!(resumed.inbox.next_room().await.unwrap().id(), 0);
    assert_eq!(resumed.inbox.next_room().await.unwrap().id(), room_id);
    let Some(ClientEvent::Frame(snapshot)) = resumed.inbox.recv().await else {
        panic!("missing snapshot")
    };
    assert_eq!(
        study_server::protocol::GameFrame::decode(snapshot)
            .unwrap()
            .payload,
        1u64.to_be_bytes()[..]
    );
    e.suspend(id, a.generation).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(resumed.handle.is_current(resumed.generation));
    room.packet_current(id, a.generation, 98, Bytes::from_static(b"stale input"))
        .unwrap();
    room.packet_current(
        id,
        resumed.generation,
        99,
        Bytes::from_static(b"still here"),
    )
    .unwrap();
    let Some(ClientEvent::Frame(frame)) = resumed.inbox.recv().await else {
        panic!("missing reply")
    };
    assert_eq!(
        study_server::protocol::GameFrame::decode(frame)
            .unwrap()
            .opcode,
        99
    );
    e.suspend(id, resumed.generation).await;
    assert!(matches!(
        e.resume(id, &a.resume_token, None).await,
        Err(Error::AuthFailed)
    ));
    tokio::time::timeout(Duration::from_secs(1), async {
        while {
            let stats = e.stats().await.unwrap();
            stats.sessions != 0 || stats.rooms != 1
        } {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(e.stats().await.unwrap().rooms, 1);
    e.shutdown().await;
}

#[tokio::test]
async fn account_resume_requires_fresh_matching_identity_and_retains_capacity() {
    let e = Engine::new(
        Config {
            max_sessions: 1,
            ..Config::default()
        },
        |_| Box::new(EchoGame),
    )
    .await
    .unwrap();
    let identity = Identity {
        principal: Principal::Account(uuid::Uuid::new_v4()),
        name: "account".into(),
        avatar_url: None,
    };
    let a = e.connect_identity(identity.clone()).await.unwrap();
    e.suspend(a.handle.id(), a.generation).await;
    let Principal::Account(account) = identity.principal else {
        unreachable!()
    };
    assert_eq!(
        e.sessions().online(vec![account]).await.unwrap(),
        vec![false]
    );
    assert!(matches!(
        e.connect(Principal::guest(), "full".into()).await,
        Err(Error::Limit)
    ));
    assert!(matches!(
        e.resume(a.handle.id(), &a.resume_token, None).await,
        Err(Error::AuthFailed)
    ));
    assert!(matches!(
        e.resume(
            a.handle.id(),
            &a.resume_token,
            Some(Identity {
                principal: Principal::guest(),
                ..identity.clone()
            })
        )
        .await,
        Err(Error::AuthFailed)
    ));
    let b = e
        .resume(a.handle.id(), &a.resume_token, Some(identity))
        .await
        .unwrap();
    assert_eq!(
        e.sessions().online(vec![account]).await.unwrap(),
        vec![true]
    );
    e.disconnect(b.handle.id()).await;
    assert_eq!(e.stats().await.unwrap().sessions, 0);
    e.shutdown().await;
}

#[tokio::test]
async fn manual_entry_cancels_queue_and_control_policy_is_enforced() {
    let e = Engine::new(Config::default(), |_| Box::new(EchoGame))
        .await
        .unwrap();
    let a = e
        .connect(Principal::guest(), "queued".into())
        .await
        .unwrap();
    e.queue(&a.handle).await.unwrap();
    e.create(&a.handle, 2).await.unwrap();
    assert_eq!(e.stats().await.unwrap().queued, 0);
    e.shutdown().await;
    let e = Engine::new(
        Config {
            allow_create: false,
            allow_join: false,
            allow_queue: false,
            ..Config::default()
        },
        |_| Box::new(EchoGame),
    )
    .await
    .unwrap();
    let mut a = e
        .connect(Principal::guest(), "denied".into())
        .await
        .unwrap();
    let global = a.inbox.next_room().await.unwrap();
    assert_eq!(
        global.packet(a.handle.id(), 1, Bytes::new()),
        Err(Error::Denied)
    );
    assert_eq!(e.create(&a.handle, 2).await, Err(Error::Denied));
    assert_eq!(e.join(&a.handle, 42).await, Err(Error::Denied));
    assert_eq!(e.queue(&a.handle).await, Err(Error::Denied));
    e.shutdown().await;
}

#[tokio::test]
async fn authenticated_active_resume_replaces_blackholed_transport_before_detection() {
    let e = Engine::new(Config::default(), |_| Box::new(EchoGame))
        .await
        .unwrap();
    let mut old = e
        .connect(Principal::guest(), "mobile".into())
        .await
        .unwrap();
    old.inbox.next_room().await.unwrap();
    let room_id = e.create(&old.handle, 2).await.unwrap();
    let room = old.inbox.next_room().await.unwrap();
    let mut new = e
        .resume(old.handle.id(), &old.resume_token, None)
        .await
        .unwrap();
    assert!(old.inbox.recv().await.is_none());
    assert_eq!(new.inbox.next_room().await.unwrap().id(), 0);
    assert_eq!(new.inbox.next_room().await.unwrap().id(), room_id);
    e.suspend(old.handle.id(), old.generation).await;
    assert!(new.handle.is_current(new.generation));
    room.packet_current(
        new.handle.id(),
        new.generation,
        1,
        Bytes::from_static(b"taken over"),
    )
    .unwrap();
    let Some(ClientEvent::Frame(frame)) = new.inbox.recv().await else {
        panic!("no reply")
    };
    assert_eq!(
        study_server::protocol::GameFrame::decode(frame)
            .unwrap()
            .payload,
        b"taken over"[..]
    );
    assert!(matches!(
        e.resume(old.handle.id(), &old.resume_token, None).await,
        Err(Error::AuthFailed)
    ));
    e.shutdown().await;
}

#[tokio::test]
async fn suspended_slots_and_lifecycle_hooks_survive_until_final_cleanup() {
    use std::sync::{Arc, Mutex};
    struct Audit(Arc<Mutex<Vec<&'static str>>>);
    impl GameLogic for Audit {
        fn on_event(&mut self, event: GameEvent, ctx: &mut RoomContext<'_>) {
            if ctx.room_id() == 0 {
                return;
            }
            let name = match event {
                GameEvent::Joined(_) => "joined",
                GameEvent::Disconnected(_) => "disconnected",
                GameEvent::Reconnected(player) => {
                    assert_eq!(ctx.host(), Some(player.session_id));
                    "reconnected"
                }
                GameEvent::Left(_) => "left",
                GameEvent::Closing => "closing",
                _ => return,
            };
            self.0.lock().unwrap().push(name);
        }
    }
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = events.clone();
    let e = Engine::new(
        Config {
            reconnect_grace: Duration::from_millis(30),
            ..Config::default()
        },
        move |_| Box::new(Audit(observed.clone())),
    )
    .await
    .unwrap();
    let a = e.connect(Principal::guest(), "host".into()).await.unwrap();
    let b = e.connect(Principal::guest(), "peer".into()).await.unwrap();
    let c = e
        .connect(Principal::guest(), "waiting".into())
        .await
        .unwrap();
    let room = e.create(&a.handle, 2).await.unwrap();
    e.queue(&b.handle).await.unwrap();
    e.join(&b.handle, room).await.unwrap();
    assert_eq!(e.stats().await.unwrap().queued, 0);
    e.suspend(a.handle.id(), a.generation).await;
    assert_eq!(e.join(&c.handle, room).await, Err(Error::RoomFull));
    let resumed = e
        .resume(a.handle.id(), &a.resume_token, None)
        .await
        .unwrap();
    // A dropped embedded transport must follow the same grace cleanup path.
    drop(b);
    tokio::time::timeout(Duration::from_secs(1), async {
        while e.stats().await.unwrap().sessions != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    e.disconnect(resumed.handle.id()).await;
    e.shutdown().await;
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            "joined",
            "joined",
            "disconnected",
            "reconnected",
            "disconnected",
            "left",
            "left",
            "closing"
        ]
    );
}

#[tokio::test]
async fn cold_controls_reject_every_operation_from_a_replaced_generation() {
    use study_server::protocol::Operation;
    let e = Engine::new(Config::default(), |_| Box::new(EchoGame))
        .await
        .unwrap();
    let old = e.connect(Principal::guest(), "old".into()).await.unwrap();
    let peer = e.connect(Principal::guest(), "peer".into()).await.unwrap();
    let peer_room = e.create(&peer.handle, 2).await.unwrap();
    let new = e
        .resume(old.handle.id(), &old.resume_token, None)
        .await
        .unwrap();
    for operation in [
        Operation::Create { capacity: 2 },
        Operation::Join { room_id: peer_room },
        Operation::Leave { room_id: peer_room },
        Operation::Queue,
        Operation::CancelQueue,
    ] {
        assert_eq!(
            e.control_current(&old.handle, old.generation, operation)
                .await,
            Err(Error::Closed)
        );
    }
    let own_room = e
        .control_current(
            &new.handle,
            new.generation,
            Operation::Create { capacity: 2 },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        e.control_current(
            &old.handle,
            old.generation,
            Operation::Leave { room_id: own_room }
        )
        .await,
        Err(Error::Closed)
    );
    assert_eq!(e.stats().await.unwrap().rooms, 3);
    assert_eq!(
        e.control_current(
            &new.handle,
            new.generation,
            Operation::Leave { room_id: own_room }
        )
        .await,
        Ok(Some(own_room))
    );
    e.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_inflight_control_commits_before_resume_can_rotate_generation() {
    use std::sync::{Arc, Condvar, Mutex};
    use study_server::protocol::Operation;
    struct Admission {
        entered: tokio::sync::mpsc::UnboundedSender<()>,
        release: Arc<(Mutex<bool>, Condvar)>,
    }
    impl GameLogic for Admission {
        fn admit(&self, _: &study_server::types::Player) -> bool {
            self.entered.send(()).unwrap();
            let (mutex, ready) = &*self.release;
            let mut released = mutex.lock().unwrap();
            while !*released {
                released = ready.wait(released).unwrap();
            }
            true
        }
        fn on_event(&mut self, _: GameEvent, _: &mut RoomContext<'_>) {}
    }
    let (entered, mut wait) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let game_release = release.clone();
    let e = Engine::new(Config::default(), move |id| {
        if id == 0 {
            Box::new(EchoGame)
        } else {
            Box::new(Admission {
                entered: entered.clone(),
                release: game_release.clone(),
            })
        }
    })
    .await
    .unwrap();
    let old = e.connect(Principal::guest(), "old".into()).await.unwrap();
    let operation_engine = e.clone();
    let operation_client = old.handle.clone();
    let generation = old.generation;
    let operation = tokio::spawn(async move {
        operation_engine
            .control_current(
                &operation_client,
                generation,
                Operation::Create { capacity: 2 },
            )
            .await
    });
    wait.recv().await.unwrap();
    operation.abort();
    let resume_engine = e.clone();
    let session = old.handle.id();
    let token = old.resume_token.clone();
    let mut resume = tokio::spawn(async move { resume_engine.resume(session, &token, None).await });
    let blocked = tokio::time::timeout(Duration::from_millis(30), &mut resume)
        .await
        .is_err();
    let still_current = old.handle.is_current(generation);
    // Always release the callback before assertions, including the red test path.
    let (mutex, ready) = &*release;
    *mutex.lock().unwrap() = true;
    ready.notify_all();
    let mut new = resume.await.unwrap().unwrap();
    assert!(
        blocked && still_current,
        "resume rotated while an old control was still committing"
    );
    assert_eq!(new.inbox.next_room().await.unwrap().id(), 0);
    assert_ne!(new.inbox.next_room().await.unwrap().id(), 0);
    assert!(new.handle.is_current(new.generation));
    assert_eq!(e.stats().await.unwrap().rooms, 2);
    e.shutdown().await;
}
