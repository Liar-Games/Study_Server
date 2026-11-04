use crate::{
    Error, Result,
    actors::{
        client::{ClientHandle, ConnectedClient},
        matchmaker::Matchmaker,
        room_manager::RoomManager,
        session_manager::SessionManager,
    },
    game::GameLogic,
    protocol::Operation,
    types::{Encounter, Identity, Player, Principal, RoomId, SessionId},
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{sync::Arc, time::Duration};
use tokio::sync::{Mutex, mpsc, watch};

#[derive(Clone, Debug)]
pub struct Config {
    pub max_sessions: usize,
    pub max_rooms: usize,
    pub max_room_players: usize,
    pub max_rooms_per_session: usize,
    pub room_capacity: usize,
    pub outbound_capacity: usize,
    pub max_frame: usize,
    pub match_size: usize,
    pub messages_per_second: u32,
    pub auth_timeout: Duration,
    pub write_timeout: Duration,
    /// Legacy configuration compatibility. Transport liveness uses Ping/Pong.
    pub idle_timeout: Duration,
    pub reconnect_grace: Duration,
    pub ping_interval: Duration,
    pub pong_timeout: Duration,
    pub allow_global_input: bool,
    pub allow_create: bool,
    pub allow_join: bool,
    pub allow_queue: bool,
    pub allowed_origins: Vec<String>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            max_sessions: 10_000,
            max_rooms: 2_000,
            max_room_players: 64,
            max_rooms_per_session: 2,
            room_capacity: 256,
            outbound_capacity: 128,
            max_frame: 65_536,
            match_size: 2,
            messages_per_second: 120,
            auth_timeout: Duration::from_secs(5),
            write_timeout: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(60),
            reconnect_grace: Duration::from_secs(60),
            ping_interval: Duration::from_secs(20),
            pong_timeout: Duration::from_secs(10),
            allow_global_input: false,
            allow_create: true,
            allow_join: true,
            allow_queue: true,
            allowed_origins: Vec::new(),
        }
    }
}
#[derive(Debug, serde::Serialize)]
pub struct Stats {
    pub sessions: usize,
    pub rooms: usize,
    pub queued: usize,
    pub dropped_encounters: u64,
}
struct Inner {
    config: Arc<Config>,
    dropped_encounters: Arc<AtomicU64>,
    sessions: SessionManager,
    rooms: RoomManager,
    matches: Matchmaker,
    stop: watch::Sender<bool>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    lifecycle: Mutex<()>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}
#[derive(Clone)]
pub struct Engine(Arc<Inner>);
impl Engine {
    pub async fn new(
        config: Config,
        factory: impl Fn(RoomId) -> Box<dyn GameLogic> + Send + Sync + 'static,
    ) -> Result<Self> {
        Self::start(config, factory, None).await
    }
    pub async fn with_encounters(
        config: Config,
        factory: impl Fn(RoomId) -> Box<dyn GameLogic> + Send + Sync + 'static,
        encounters: mpsc::Sender<Encounter>,
    ) -> Result<Self> {
        Self::start(config, factory, Some(encounters)).await
    }
    async fn start(
        config: Config,
        factory: impl Fn(RoomId) -> Box<dyn GameLogic> + Send + Sync + 'static,
        encounters: Option<mpsc::Sender<Encounter>>,
    ) -> Result<Self> {
        if config.max_sessions == 0
            || config.max_rooms < 1
            || config.max_room_players == 0
            || config.max_rooms_per_session < 2
            || config.room_capacity == 0
            || config.outbound_capacity < 2
            || config.max_frame < 64
            || config.match_size > config.max_room_players
            || config.messages_per_second == 0
            || config.auth_timeout.is_zero()
            || config.write_timeout.is_zero()
            || config.idle_timeout.is_zero()
            || config.reconnect_grace.is_zero()
            || config.ping_interval.is_zero()
            || config.pong_timeout.is_zero()
        {
            return Err(Error::Limit);
        }
        let config = Arc::new(config);
        let (stop, stopped) = watch::channel(false);
        let (sessions, a) =
            SessionManager::start(config.max_sessions, config.reconnect_grace, stopped.clone());
        let dropped_encounters = Arc::new(AtomicU64::new(0));
        let (rooms, b) = RoomManager::start(
            config.clone(),
            Arc::new(factory),
            stopped.clone(),
            stop.clone(),
            encounters,
            dropped_encounters.clone(),
        );
        let (matches, c) = Matchmaker::start(
            rooms.clone(),
            config.match_size,
            config.max_sessions,
            stopped,
        );
        let engine = Self(Arc::new(Inner {
            config,
            dropped_encounters,
            sessions,
            rooms,
            matches,
            stop,
            tasks: Mutex::new(vec![a, b, c]),
            lifecycle: Mutex::new(()),
        }));
        let weak = Arc::downgrade(&engine.0);
        let mut stopped = engine.0.stop.subscribe();
        let period = engine.config().reconnect_grace.min(Duration::from_secs(1));
        let expiry = tokio::spawn(async move {
            let mut timer = tokio::time::interval(period);
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! { biased; _ = stopped.changed() => break, _ = timer.tick() => {} }
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                let _guard = inner.lifecycle.lock().await;
                let (disconnected, expired) = inner.sessions.expire().await;
                for id in disconnected {
                    let _ = inner.matches.leave(id).await;
                    let _ = inner.rooms.connection(id, false).await;
                }
                for id in expired {
                    let _ = inner.matches.leave(id).await;
                    inner.rooms.disconnect(id).await;
                }
            }
        });
        engine.0.tasks.lock().await.push(expiry);
        // Startup barrier: global room exists before accepting any connection.
        engine.0.rooms.count().await?;
        Ok(engine)
    }
    pub fn config(&self) -> &Config {
        &self.0.config
    }
    pub fn sessions(&self) -> &SessionManager {
        &self.0.sessions
    }
    pub fn is_running(&self) -> bool {
        !*self.0.stop.borrow()
    }
    pub async fn connect(&self, principal: Principal, name: String) -> Result<ConnectedClient> {
        self.connect_identity(Identity {
            principal,
            name,
            avatar_url: None,
        })
        .await
    }
    pub async fn connect_identity(&self, identity: Identity) -> Result<ConnectedClient> {
        let Identity {
            principal,
            name,
            avatar_url,
        } = identity;
        if !self.is_running() {
            return Err(Error::Unavailable);
        }
        if name.len() > 256 || avatar_url.as_ref().is_some_and(|url| url.len() > 2048) {
            return Err(Error::Limit);
        }
        let client = ClientHandle::channel(
            Player {
                session_id: uuid::Uuid::new_v4(),
                principal,
                name,
                avatar_url,
            },
            self.config().outbound_capacity,
            self.config().max_frame,
        );
        self.0
            .sessions
            .register(client.handle.clone(), client.resume_token.clone())
            .await?;
        if let Err(error) = self.0.rooms.join(client.handle.clone(), 0).await {
            self.disconnect(client.handle.id()).await;
            return Err(error);
        }
        Ok(client)
    }
    /// Suspend only the transport generation that reported the disconnection.
    pub async fn suspend(&self, id: SessionId, generation: u64) {
        let Ok(mut clients) = self.0.sessions.resolve(vec![id]).await else {
            return;
        };
        let Some(client) = clients.pop() else {
            return;
        };
        let _control = client.control_guard().await;
        let _guard = self.0.lifecycle.lock().await;
        if !self.0.sessions.suspend(id, generation).await {
            return;
        }
        let _ = self.0.matches.leave(id).await;
        let _ = self.0.rooms.connection(id, false).await;
    }
    /// Accounts must present a fresh authenticated identity for the same account.
    /// Guests must present the private rotating resume token and no identity.
    pub async fn resume(
        &self,
        id: SessionId,
        token: &str,
        identity: Option<Identity>,
    ) -> Result<ConnectedClient> {
        let client = self
            .0
            .sessions
            .resolve(vec![id])
            .await?
            .pop()
            .ok_or(Error::AuthFailed)?;
        let _control = client.control_guard().await;
        let _guard = self.0.lifecycle.lock().await;
        if !self.is_running() {
            return Err(Error::Unavailable);
        }
        let mut client = self.0.sessions.resume(id, token, identity).await?;
        let routes = self.0.rooms.routes(id).await?;
        client.inbox.restore_routes(routes);
        self.0.rooms.connection(id, true).await?;
        Ok(client)
    }
    pub async fn disconnect(&self, id: SessionId) {
        let client = self
            .0
            .sessions
            .resolve(vec![id])
            .await
            .ok()
            .and_then(|mut clients| clients.pop());
        let _control = match &client {
            Some(client) => Some(client.control_guard().await),
            None => None,
        };
        let _guard = self.0.lifecycle.lock().await;
        self.0.sessions.remove(id).await;
        let _ = self.0.matches.leave(id).await;
        self.0.rooms.disconnect(id).await;
    }
    /// Apply one transport control only while its connection generation is current.
    /// The per-session gate orders actor commits before connection takeover. Once
    /// started, a commit retains its gate even if the transport caller is cancelled.
    pub async fn control_current(
        &self,
        client: &ClientHandle,
        generation: u64,
        operation: Operation,
    ) -> Result<Option<RoomId>> {
        let guard = client.control_guard().await;
        if !client.is_current(generation) {
            return Err(Error::Closed);
        }
        let engine = self.clone();
        let client = client.clone();
        tokio::spawn(async move {
            let _guard = guard;
            if !client.is_current(generation) {
                return Err(Error::Closed);
            }
            match operation {
                Operation::Create { capacity } => engine.create(&client, capacity).await.map(Some),
                Operation::Join { room_id } => {
                    engine.join(&client, room_id).await.map(|()| Some(room_id))
                }
                Operation::Leave { room_id } => {
                    engine.leave(&client, room_id).await.map(|()| Some(room_id))
                }
                Operation::Queue => engine.queue(&client).await.map(|()| None),
                Operation::CancelQueue => engine.cancel_queue(client.id()).await.map(|()| None),
            }
        })
        .await
        .map_err(|_| Error::Unavailable)?
    }
    pub async fn create(&self, client: &ClientHandle, capacity: usize) -> Result<RoomId> {
        if !self.config().allow_create {
            return Err(Error::Denied);
        }
        self.0.matches.leave(client.id()).await?;
        self.0.rooms.create(vec![client.clone()], capacity).await
    }
    pub async fn join(&self, client: &ClientHandle, room: RoomId) -> Result<()> {
        if !self.config().allow_join {
            return Err(Error::Denied);
        }
        self.0.matches.leave(client.id()).await?;
        self.0.rooms.join(client.clone(), room).await
    }
    pub async fn leave(&self, client: &ClientHandle, room: RoomId) -> Result<()> {
        self.0.rooms.leave(client.id(), room).await
    }
    pub async fn queue(&self, client: &ClientHandle) -> Result<()> {
        if !self.config().allow_queue {
            return Err(Error::Denied);
        }
        if !self.0.rooms.eligible(client).await? {
            return Err(Error::Denied);
        }
        self.0.matches.join(client.clone()).await
    }
    pub async fn cancel_queue(&self, id: SessionId) -> Result<()> {
        self.0.matches.leave(id).await
    }
    pub async fn stats(&self) -> Result<Stats> {
        Ok(Stats {
            sessions: self.0.sessions.count().await?,
            rooms: self.0.rooms.count().await?,
            queued: self.0.matches.count().await?,
            dropped_encounters: self.0.dropped_encounters.load(Ordering::Relaxed),
        })
    }
    pub async fn shutdown(&self) {
        self.0.stop.send_replace(true);
        for task in self.0.tasks.lock().await.drain(..) {
            let _ = task.await;
        }
    }
}
