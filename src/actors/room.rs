use crate::{
    Error, Result,
    actors::client::{ClientEvent, ClientHandle},
    game::{GameEvent, GameLogic, RoomContext},
    protocol,
    types::{RoomId, SessionId},
};
use bytes::Bytes;
use std::{collections::HashMap, time::Duration};
use tokio::sync::{mpsc, oneshot, watch};

enum Command {
    Join(ClientHandle, oneshot::Sender<Result<()>>),
    Leave(SessionId, oneshot::Sender<()>),
    Packet(SessionId, Option<u64>, u16, Bytes),
    Connection(SessionId, bool, oneshot::Sender<()>),
    Event(GameEvent),
}
#[derive(Clone, Debug)]
pub struct RoomHandle {
    id: RoomId,
    tx: mpsc::Sender<Command>,
    stop: watch::Sender<bool>,
    max_payload: usize,
    allow_input: bool,
}
impl RoomHandle {
    pub fn id(&self) -> RoomId {
        self.id
    }
    pub fn is_closed(&self) -> bool {
        *self.stop.borrow() || self.tx.is_closed()
    }
    pub fn packet(&self, session: SessionId, opcode: u16, payload: Bytes) -> Result<()> {
        if !self.allow_input {
            return Err(Error::Denied);
        }
        if payload.len() > self.max_payload {
            return Err(Error::Limit);
        }
        self.enqueue(Command::Packet(session, None, opcode, payload))
    }
    /// Transport input is checked again when the room consumes the command.
    pub fn packet_current(
        &self,
        session: SessionId,
        generation: u64,
        opcode: u16,
        payload: Bytes,
    ) -> Result<()> {
        if !self.allow_input {
            return Err(Error::Denied);
        }
        if payload.len() > self.max_payload {
            return Err(Error::Limit);
        }
        self.enqueue(Command::Packet(session, Some(generation), opcode, payload))
    }
    pub(crate) async fn connection(&self, id: SessionId, connected: bool) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::Connection(id, connected, tx))
            .await
            .map_err(|_| Error::RoomNotFound)?;
        rx.await.map_err(|_| Error::RoomNotFound)
    }
    /// Trusted application worker event. Never expose this method to wire input.
    pub fn event(&self, event: GameEvent) -> Result<()> {
        self.enqueue(Command::Event(event))
    }
    fn enqueue(&self, command: Command) -> Result<()> {
        if self.is_closed() {
            return Err(Error::RoomNotFound);
        }
        self.tx.try_send(command).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => Error::Busy,
            _ => Error::RoomNotFound,
        })
    }
    pub(crate) async fn join(&self, client: ClientHandle) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::Join(client, tx))
            .await
            .map_err(|_| Error::RoomNotFound)?;
        rx.await.map_err(|_| Error::RoomNotFound)?
    }
    pub(crate) async fn leave(&self, id: SessionId) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Command::Leave(id, tx)).await.is_ok() {
            let _ = rx.await;
        }
    }
    pub(crate) fn stop(&self) {
        self.stop.send_replace(true);
    }
}

pub(crate) struct Room {
    handle: RoomHandle,
    rx: mpsc::Receiver<Command>,
    stopped: watch::Receiver<bool>,
    capacity: usize,
    host: Option<SessionId>,
    members: HashMap<SessionId, ClientHandle>,
    logic: Box<dyn GameLogic>,
    closing: bool,
}
impl Room {
    pub fn new(
        id: RoomId,
        capacity: usize,
        mailbox: usize,
        max_payload: usize,
        host: Option<SessionId>,
        logic: Box<dyn GameLogic>,
        allow_input: bool,
    ) -> (Self, RoomHandle) {
        let (tx, rx) = mpsc::channel(mailbox);
        let (stop, stopped) = watch::channel(false);
        let handle = RoomHandle {
            id,
            tx,
            stop,
            max_payload,
            allow_input,
        };
        (
            Self {
                handle: handle.clone(),
                rx,
                stopped,
                capacity,
                host,
                members: HashMap::new(),
                logic,
                closing: false,
            },
            handle,
        )
    }
    fn dispatch(&mut self, event: GameEvent) {
        let mut ctx = RoomContext {
            handle: &self.handle,
            id: self.handle.id,
            host: self.host,
            members: &self.members,
            closing: &mut self.closing,
        };
        self.logic.on_event(event, &mut ctx);
    }
    pub async fn run(mut self) {
        let tick = self.logic.tick_interval();
        let mut timer = tokio::time::interval(
            tick.unwrap_or(Duration::from_secs(3600))
                .max(Duration::from_millis(1)),
        );
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        timer.tick().await;
        loop {
            if *self.stopped.borrow() {
                break;
            }
            tokio::select! {
                _ = self.stopped.changed() => break,
                cmd = self.rx.recv() => match cmd {
                    Some(Command::Join(client, reply)) => {
                        let id = client.id();
                        let already_joined = self.members.contains_key(&id);
                        let result = if client.is_closed() { Err(Error::Closed) }
                        else if self.members.contains_key(&id) { Ok(()) }
                        else if self.members.len() >= self.capacity { Err(Error::RoomFull) }
                        else if !self.logic.admit(client.player()) { Err(Error::Denied) }
                        else {
                            client.deliver(ClientEvent::Joined(self.handle.clone())).map(|()| {
                                let player = client.player().clone();
                                self.members.insert(id, client);
                                self.dispatch(GameEvent::Joined(player));
                            })
                        };
                        if reply.send(result).is_err() && !already_joined && self.members.remove(&id).is_some() {
                            self.dispatch(GameEvent::Left(id));
                        }
                    },
                    Some(Command::Leave(id, reply)) => {
                        if let Some(client) = self.members.remove(&id) {
                            let _ = client.deliver(ClientEvent::Left(self.handle.id));
                            if self.host == Some(id) { self.host = None; }
                            self.dispatch(GameEvent::Left(id));
                        }
                        let _ = reply.send(());
                        if self.handle.id != 0 && self.members.is_empty() { break; }
                    },
                    Some(Command::Connection(id, connected, reply)) => {
                        if let Some(client) = self.members.get(&id) {
                            let event = if connected { GameEvent::Reconnected(client.player().clone()) } else { GameEvent::Disconnected(id) };
                            self.dispatch(event);
                        }
                        let _ = reply.send(());
                    },
                    Some(Command::Packet(id, generation, opcode, payload)) => {
                        if self.handle.allow_input && self.members.get(&id).is_some_and(|c| generation.map_or_else(|| !c.is_closed(), |g| c.is_current(g))) {
                            self.dispatch(GameEvent::Message { player: id, opcode, payload });
                        }
                    },
                    Some(Command::Event(event)) => self.dispatch(event),
                    None => break,
                },
                _ = timer.tick(), if tick.is_some() => self.dispatch(GameEvent::Tick),
            }
            if self.closing {
                break;
            }
        }
        self.dispatch(GameEvent::Closing);
        self.handle.stop();
        // Manager sends the lifecycle notification and removes routing indexes.
    }
}

pub(crate) fn notify_closed(client: &ClientHandle, room: RoomId) {
    let _ = client.deliver(ClientEvent::Left(room));
    let _ = client.send(protocol::json(
        protocol::ROOM_CLOSED,
        &serde_json::json!({"room_id": room}),
    ));
}
