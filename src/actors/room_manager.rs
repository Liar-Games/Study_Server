use crate::{
    Error, Result,
    actors::{
        client::ClientHandle,
        room::{Room, RoomHandle, notify_closed},
    },
    engine::Config,
    game::GameLogic,
    types::{Encounter, RoomId, SessionId},
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::{Id, JoinSet},
};

use std::sync::atomic::{AtomicU64, Ordering};

pub type GameFactory = Arc<dyn Fn(RoomId) -> Box<dyn GameLogic> + Send + Sync>;
enum Command {
    Create(Vec<ClientHandle>, usize, oneshot::Sender<Result<RoomId>>),
    Join(ClientHandle, RoomId, oneshot::Sender<Result<()>>),
    Leave(SessionId, RoomId, oneshot::Sender<Result<()>>),
    Disconnect(SessionId, oneshot::Sender<()>),
    Count(oneshot::Sender<usize>),
    Eligible(ClientHandle, oneshot::Sender<bool>),
    MatchCreate(
        Vec<ClientHandle>,
        usize,
        oneshot::Sender<std::result::Result<RoomId, MatchFailure>>,
    ),
    Routes(SessionId, oneshot::Sender<Vec<RoomHandle>>),
    Connection(SessionId, bool, oneshot::Sender<Result<()>>),
}
pub(crate) struct MatchFailure {
    pub error: Error,
    pub disqualified: Vec<SessionId>,
}
#[derive(Clone)]
pub struct RoomManager {
    tx: mpsc::Sender<Command>,
}
struct Record {
    handle: RoomHandle,
    members: HashMap<SessionId, ClientHandle>,
}
struct State {
    config: Arc<Config>,
    encounters: Option<mpsc::Sender<Encounter>>,
    dropped_encounters: Arc<AtomicU64>,
    factory: GameFactory,
    next_id: RoomId,
    rooms: HashMap<RoomId, Record>,
    membership: HashMap<SessionId, HashSet<RoomId>>,
    tasks: JoinSet<()>,
    task_rooms: HashMap<Id, RoomId>,
}
impl State {
    fn encounter(&self, client: &ClientHandle, peers: Vec<ClientHandle>) {
        if peers.is_empty() {
            return;
        }
        if let Some(tx) = &self.encounters {
            let event = Encounter {
                player: client.player().clone(),
                peers: peers
                    .iter()
                    .filter(|p| !p.is_closed())
                    .map(|p| p.player().clone())
                    .collect(),
            };
            if !event.peers.is_empty() && tx.try_send(event).is_err() {
                self.dropped_encounters.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn spawn(&mut self, id: RoomId, capacity: usize, host: Option<SessionId>) -> Result<()> {
        let logic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.factory)(id)))
            .map_err(|_| Error::Unavailable)?;
        let (room, handle) = Room::new(
            id,
            capacity,
            self.config.room_capacity,
            self.config.max_frame - 11,
            host,
            logic,
            id != 0 || self.config.allow_global_input,
        );
        let task = self.tasks.spawn(room.run());
        self.task_rooms.insert(task.id(), id);
        self.rooms.insert(
            id,
            Record {
                handle,
                members: HashMap::new(),
            },
        );
        Ok(())
    }
    async fn join(&mut self, client: ClientHandle, id: RoomId) -> Result<()> {
        if client.is_closed() {
            return Err(Error::Closed);
        }
        if let Some(joined) = self.membership.get(&client.id()) {
            if joined.contains(&id) {
                return Ok(());
            }
            if joined.len() >= self.config.max_rooms_per_session {
                return Err(Error::Limit);
            }
        }
        let record = self.rooms.get_mut(&id).ok_or(Error::RoomNotFound)?;
        if let Err(error) = record.handle.join(client.clone()).await {
            // Joined may have been published before a game callback panicked.
            let _ = client.deliver(crate::actors::client::ClientEvent::Left(id));
            return Err(error);
        }
        record.members.insert(client.id(), client.clone());
        self.membership.entry(client.id()).or_default().insert(id);
        Ok(())
    }
    fn eligible(&self, client: &ClientHandle) -> bool {
        !client.is_closed()
            && self
                .membership
                .get(&client.id())
                .is_some_and(|rooms| rooms.contains(&0) && rooms.iter().all(|id| *id == 0))
    }
    async fn match_create(
        &mut self,
        clients: Vec<ClientHandle>,
        capacity: usize,
    ) -> std::result::Result<RoomId, MatchFailure> {
        let disqualified: Vec<_> = clients
            .iter()
            .filter(|client| !self.eligible(client))
            .map(ClientHandle::id)
            .collect();
        if !disqualified.is_empty() {
            return Err(MatchFailure {
                error: Error::Denied,
                disqualified,
            });
        }
        // The same actor owns this check and every membership mutation, closing the
        // race with manual joins between queue qualification and room creation.
        match self.create(clients.clone(), capacity).await {
            Ok(id) => Ok(id),
            Err(error) => Err(MatchFailure {
                error,
                disqualified: clients
                    .iter()
                    .filter(|client| !self.eligible(client))
                    .map(ClientHandle::id)
                    .collect(),
            }),
        }
    }
    async fn create(&mut self, clients: Vec<ClientHandle>, capacity: usize) -> Result<RoomId> {
        if capacity == 0
            || capacity > self.config.max_room_players
            || clients.is_empty()
            || clients.len() > capacity
        {
            return Err(Error::Limit);
        }
        if self.rooms.len() >= self.config.max_rooms {
            return Err(Error::Limit);
        }
        let id = self.next_id;
        self.next_id = id.checked_add(1).ok_or(Error::Limit)?;
        self.spawn(id, capacity, Some(clients[0].id()))?;
        for client in &clients {
            if let Err(error) = self.join(client.clone(), id).await {
                self.remove_room(id);
                return Err(error);
            }
        }
        if self.encounters.is_some() {
            for (index, client) in clients.iter().enumerate() {
                self.encounter(client, clients[..index].to_vec());
            }
        }
        Ok(id)
    }
    fn remove_room(&mut self, id: RoomId) {
        if let Some(record) = self.rooms.remove(&id) {
            record.handle.stop();
            for (session, client) in record.members {
                if let Some(joined) = self.membership.get_mut(&session) {
                    joined.remove(&id);
                    if joined.is_empty() {
                        self.membership.remove(&session);
                    }
                }
                notify_closed(&client, id);
                if id == 0 {
                    client.close();
                }
            }
        }
    }
    async fn leave(&mut self, session: SessionId, id: RoomId) {
        if let Some(record) = self.rooms.get_mut(&id) {
            if record.members.remove(&session).is_some() {
                record.handle.leave(session).await;
            }
            if id != 0 && record.members.is_empty() {
                self.remove_room(id);
            }
        }
        if let Some(joined) = self.membership.get_mut(&session) {
            joined.remove(&id);
            if joined.is_empty() {
                self.membership.remove(&session);
            }
        }
    }
}
impl RoomManager {
    pub(crate) fn start(
        config: Arc<Config>,
        factory: GameFactory,
        mut stop: watch::Receiver<bool>,
        shutdown: watch::Sender<bool>,
        encounters: Option<mpsc::Sender<Encounter>>,
        dropped_encounters: Arc<AtomicU64>,
    ) -> (Self, tokio::task::JoinHandle<()>) {
        let (tx, mut rx) = mpsc::channel(128);
        let task = tokio::spawn(async move {
            let mut state = State {
                config,
                encounters,
                dropped_encounters,
                factory,
                next_id: 1,
                rooms: HashMap::new(),
                membership: HashMap::new(),
                tasks: JoinSet::new(),
                task_rooms: HashMap::new(),
            };
            if state.spawn(0, state.config.max_sessions, None).is_err() {
                return;
            }
            loop {
                tokio::select! {
                    biased;
                    _ = stop.changed() => break,
                    result = state.tasks.join_next_with_id(), if !state.tasks.is_empty() => {
                        let task = match result.unwrap() { Ok((id, ())) => id, Err(error) => error.id() };
                        if let Some(room) = state.task_rooms.remove(&task) {
                            state.remove_room(room);
                            if room == 0 { shutdown.send_replace(true); break; }
                        }
                    },
                    cmd = rx.recv() => match cmd {
                        Some(Command::Create(clients, capacity, reply)) => {
                            let result = state.create(clients, capacity).await;
                            if let Err(Ok(id)) = reply.send(result) { state.remove_room(id); }
                        },
                        Some(Command::Join(client, id, reply)) => {
                            let session = client.id();
                            let already_joined = state.membership.get(&session).is_some_and(|rooms| rooms.contains(&id));
                            let result = state.join(client.clone(), id).await;
                            if result.is_ok() && id != 0 && !already_joined && state.encounters.is_some() {
                                let peers = state.rooms[&id].members.values().filter(|p| p.id() != session).cloned().collect();
                                state.encounter(&client, peers);
                            }
                            if let Err(Ok(())) = reply.send(result) && !already_joined { state.leave(session, id).await; }
                        },
                        Some(Command::Leave(session, id, reply)) => {
                            let result = if id == 0 { Err(Error::Denied) }
                            else { state.leave(session, id).await; Ok(()) };
                            let _ = reply.send(result);
                        },
                        Some(Command::Disconnect(session, reply)) => {
                            let rooms = state.membership.get(&session).cloned().unwrap_or_default();
                            for id in rooms { state.leave(session, id).await; }
                            let _ = reply.send(());
                        },
                        Some(Command::Eligible(client, reply)) => { let _ = reply.send(state.eligible(&client)); },
                        Some(Command::MatchCreate(clients, capacity, reply)) => {
                            let result = state.match_create(clients, capacity).await;
                            if let Err(Ok(id)) = reply.send(result) { state.remove_room(id); }
                        },
                        Some(Command::Routes(session, reply)) => {
                            let mut ids: Vec<_> = state.membership.get(&session).into_iter().flatten().copied().collect();
                            ids.sort_unstable();
                            let routes = ids.into_iter().filter_map(|id| state.rooms.get(&id)).filter(|record| !record.handle.is_closed()).map(|record| record.handle.clone()).collect();
                            let _ = reply.send(routes);
                        },
                        Some(Command::Connection(session, connected, reply)) => {
                            let ids = state.membership.get(&session).cloned().unwrap_or_default();
                            for id in ids {
                                if let Some(record) = state.rooms.get(&id) { let _ = record.handle.connection(session, connected).await; }
                            }
                            let _ = reply.send(Ok(()));
                        },
                        Some(Command::Count(reply)) => { let _ = reply.send(state.rooms.len()); },
                        None => break,
                    }
                }
            }
            for record in state.rooms.values() {
                record.handle.stop();
            }
            while state.tasks.join_next().await.is_some() {}
        });
        (Self { tx }, task)
    }
    pub(crate) async fn eligible(&self, client: &ClientHandle) -> Result<bool> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::Eligible(client.clone(), tx))
            .await
            .map_err(|_| Error::Unavailable)?;
        rx.await.map_err(|_| Error::Unavailable)
    }
    pub(crate) async fn match_create(
        &self,
        clients: Vec<ClientHandle>,
        capacity: usize,
    ) -> std::result::Result<RoomId, MatchFailure> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::MatchCreate(clients, capacity, tx))
            .await
            .map_err(|_| MatchFailure {
                error: Error::Unavailable,
                disqualified: vec![],
            })?;
        rx.await.map_err(|_| MatchFailure {
            error: Error::Unavailable,
            disqualified: vec![],
        })?
    }
    pub(crate) async fn routes(&self, id: SessionId) -> Result<Vec<RoomHandle>> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::Routes(id, tx))
            .await
            .map_err(|_| Error::Unavailable)?;
        rx.await.map_err(|_| Error::Unavailable)
    }
    pub(crate) async fn connection(&self, id: SessionId, connected: bool) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::Connection(id, connected, tx))
            .await
            .map_err(|_| Error::Unavailable)?;
        rx.await.map_err(|_| Error::Unavailable)?
    }
    pub(crate) async fn create(
        &self,
        clients: Vec<ClientHandle>,
        capacity: usize,
    ) -> Result<RoomId> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::Create(clients, capacity, tx))
            .await
            .map_err(|_| Error::Unavailable)?;
        rx.await.map_err(|_| Error::Unavailable)?
    }
    pub(crate) async fn join(&self, client: ClientHandle, room: RoomId) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::Join(client, room, tx))
            .await
            .map_err(|_| Error::Unavailable)?;
        rx.await.map_err(|_| Error::Unavailable)?
    }
    pub(crate) async fn leave(&self, id: SessionId, room: RoomId) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::Leave(id, room, tx))
            .await
            .map_err(|_| Error::Unavailable)?;
        rx.await.map_err(|_| Error::Unavailable)?
    }
    pub(crate) async fn disconnect(&self, id: SessionId) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Command::Disconnect(id, tx)).await.is_ok() {
            let _ = rx.await;
        }
    }
    pub async fn count(&self) -> Result<usize> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::Count(tx))
            .await
            .map_err(|_| Error::Unavailable)?;
        rx.await.map_err(|_| Error::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        game::{GameEvent, RoomContext},
        types::{Player, Principal},
    };
    struct Deny;
    impl GameLogic for Deny {
        fn admit(&self, _: &Player) -> bool {
            false
        }
        fn on_event(&mut self, _: GameEvent, _: &mut RoomContext<'_>) {}
    }
    #[tokio::test]
    async fn rejected_admissions_do_not_retain_session_indexes() {
        let mut state = State {
            config: Arc::new(Config::default()),
            encounters: None,
            dropped_encounters: Arc::new(AtomicU64::new(0)),
            factory: Arc::new(|_| Box::new(Deny)),
            next_id: 1,
            rooms: HashMap::new(),
            membership: HashMap::new(),
            tasks: JoinSet::new(),
            task_rooms: HashMap::new(),
        };
        state.spawn(0, 10, None).unwrap();
        for _ in 0..20 {
            let client = ClientHandle::channel(
                Player {
                    session_id: uuid::Uuid::new_v4(),
                    principal: Principal::guest(),
                    name: "guest".into(),
                    avatar_url: None,
                },
                8,
                1024,
            );
            assert_eq!(state.join(client.handle, 0).await, Err(Error::Denied));
        }
        assert!(
            state.membership.is_empty(),
            "rejected session IDs leaked into room index"
        );
    }
    #[tokio::test]
    async fn match_creation_rechecks_membership_before_touching_valid_candidate() {
        let mut state = State {
            config: Arc::new(Config::default()),
            encounters: None,
            dropped_encounters: Arc::new(AtomicU64::new(0)),
            factory: Arc::new(|_| Box::new(crate::game::EchoGame)),
            next_id: 1,
            rooms: HashMap::new(),
            membership: HashMap::new(),
            tasks: JoinSet::new(),
            task_rooms: HashMap::new(),
        };
        state.spawn(0, 10, None).unwrap();
        let clients: Vec<_> = ["stale", "valid"]
            .into_iter()
            .map(|name| {
                ClientHandle::channel(
                    Player {
                        session_id: uuid::Uuid::new_v4(),
                        principal: Principal::guest(),
                        name: name.into(),
                        avatar_url: None,
                    },
                    8,
                    1024,
                )
            })
            .collect();
        for client in &clients {
            state.join(client.handle.clone(), 0).await.unwrap();
        }
        assert!(state.eligible(&clients[0].handle));
        assert!(state.eligible(&clients[1].handle));
        // A manual join happened after the matchmaker selected these candidates.
        state
            .create(vec![clients[0].handle.clone()], 2)
            .await
            .unwrap();
        let failure = state
            .match_create(clients.iter().map(|c| c.handle.clone()).collect(), 2)
            .await
            .err()
            .unwrap();
        assert_eq!(failure.error, Error::Denied);
        assert_eq!(failure.disqualified, vec![clients[0].handle.id()]);
        assert!(state.eligible(&clients[1].handle));
        assert_eq!(
            state.membership[&clients[1].handle.id()],
            HashSet::from([0])
        );
        assert_eq!(state.rooms.len(), 2);
    }
    #[tokio::test]
    async fn cancelled_idempotent_join_preserves_existing_global_membership() {
        let (stop, stopped) = watch::channel(false);
        let (rooms, task) = RoomManager::start(
            Arc::new(Config::default()),
            Arc::new(|_| Box::new(crate::game::EchoGame)),
            stopped,
            stop.clone(),
            None,
            Arc::new(AtomicU64::new(0)),
        );
        let client = ClientHandle::channel(
            Player {
                session_id: uuid::Uuid::new_v4(),
                principal: Principal::guest(),
                name: "existing".into(),
                avatar_url: None,
            },
            8,
            1024,
        );
        rooms.join(client.handle.clone(), 0).await.unwrap();
        let (reply, cancelled) = oneshot::channel();
        drop(cancelled);
        rooms
            .tx
            .send(Command::Join(client.handle.clone(), 0, reply))
            .await
            .unwrap();
        assert!(
            rooms.eligible(&client.handle).await.unwrap(),
            "cancelled retry removed existing global membership"
        );
        stop.send_replace(true);
        task.await.unwrap();
    }
}
