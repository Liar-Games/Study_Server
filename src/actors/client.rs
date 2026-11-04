//! A stable logical client handle; each connection owns a separate bounded inbox.
use crate::{
    Error, Result,
    actors::room::RoomHandle,
    types::{Player, RoomId, SessionId},
};
use bytes::Bytes;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, watch};

#[derive(Debug)]
pub enum ClientEvent {
    Frame(Bytes),
    Joined(RoomHandle),
    Left(RoomId),
}
#[derive(Debug)]
struct Transport {
    tx: mpsc::Sender<ClientEvent>,
    cancel: watch::Sender<bool>,
}
#[derive(Debug)]
struct State {
    generation: u64,
    transport: Option<Transport>,
    closed: bool,
}
#[derive(Clone, Debug)]
pub struct ClientHandle {
    player: Arc<Player>,
    state: Arc<Mutex<State>>,
    control: Arc<tokio::sync::Mutex<()>>,
    capacity: usize,
    max_frame: usize,
}
pub struct ClientInbox {
    rx: mpsc::Receiver<ClientEvent>,
    cancel: watch::Receiver<bool>,
    pending: VecDeque<ClientEvent>,
    capacity: usize,
}
pub struct ConnectedClient {
    pub handle: ClientHandle,
    pub inbox: ClientInbox,
    pub generation: u64,
    pub resume_token: String,
    pub resumed: bool,
}
impl ClientHandle {
    pub(crate) fn channel(player: Player, capacity: usize, max_frame: usize) -> ConnectedClient {
        let handle = Self {
            player: Arc::new(player),
            state: Arc::new(Mutex::new(State {
                generation: 0,
                transport: None,
                closed: false,
            })),
            control: Arc::new(tokio::sync::Mutex::new(())),
            capacity,
            max_frame,
        };
        handle.attach(false).expect("new client")
    }
    pub(crate) async fn control_guard(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.control.clone().lock_owned().await
    }
    pub(crate) fn attach(&self, resumed: bool) -> Result<ConnectedClient> {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return Err(Error::Closed);
        }
        if state.transport.is_some() && !resumed {
            return Err(Error::AlreadyConnected);
        }
        let generation = state.generation.checked_add(1).ok_or(Error::Limit)?;
        if let Some(old) = state.transport.take() {
            old.cancel.send_replace(true);
        }
        state.generation = generation;
        let (tx, rx) = mpsc::channel(self.capacity);
        let (cancel, cancelled) = watch::channel(false);
        state.transport = Some(Transport { tx, cancel });
        Ok(ConnectedClient {
            handle: self.clone(),
            inbox: ClientInbox {
                rx,
                cancel: cancelled,
                pending: VecDeque::new(),
                capacity: self.capacity,
            },
            generation: state.generation,
            resume_token: format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ),
            resumed,
        })
    }
    pub fn player(&self) -> &Player {
        &self.player
    }
    pub fn id(&self) -> SessionId {
        self.player.session_id
    }
    pub fn is_current(&self, generation: u64) -> bool {
        let state = self.state.lock().unwrap();
        state.generation == generation
            && state
                .transport
                .as_ref()
                .is_some_and(|t| !*t.cancel.borrow() && !t.tx.is_closed())
            && !state.closed
    }
    pub fn is_closed(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.closed
            || state
                .transport
                .as_ref()
                .is_none_or(|t| *t.cancel.borrow() || t.tx.is_closed())
    }
    /// Cancel the current transport. The transport owner must subsequently suspend it.
    pub fn close(&self) {
        if let Some(t) = &self.state.lock().unwrap().transport {
            t.cancel.send_replace(true);
        }
    }
    pub(crate) fn close_final(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        if let Some(t) = state.transport.take() {
            t.cancel.send_replace(true);
        }
    }
    pub(crate) fn suspend(&self, generation: u64) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.closed || state.generation != generation {
            return false;
        }
        if let Some(t) = state.transport.take() {
            t.cancel.send_replace(true);
            true
        } else {
            false
        }
    }
    /// Detect an abandoned inbox without letting an older connection cancel a new one.
    pub(crate) fn suspend_closed(&self) -> Option<u64> {
        let mut state = self.state.lock().unwrap();
        if state.closed
            || !state
                .transport
                .as_ref()
                .is_some_and(|t| *t.cancel.borrow() || t.tx.is_closed())
        {
            return None;
        }
        if let Some(t) = state.transport.take() {
            t.cancel.send_replace(true);
        }
        Some(state.generation)
    }
    pub fn send(&self, frame: Bytes) -> Result<()> {
        if frame.len() > self.max_frame {
            return Err(Error::Limit);
        }
        self.deliver(ClientEvent::Frame(frame))
    }
    /// Transport replies must atomically check the connection and enqueue.
    pub(crate) fn send_current(&self, generation: u64, frame: Bytes) -> Result<()> {
        if frame.len() > self.max_frame {
            return Err(Error::Limit);
        }
        self.deliver_at(Some(generation), ClientEvent::Frame(frame))
    }
    pub(crate) fn deliver(&self, event: ClientEvent) -> Result<()> {
        self.deliver_at(None, event)
    }
    fn deliver_at(&self, generation: Option<u64>, event: ClientEvent) -> Result<()> {
        let state = self.state.lock().unwrap();
        if generation.is_some_and(|generation| generation != state.generation) {
            return Err(Error::Closed);
        }
        let t = state
            .transport
            .as_ref()
            .filter(|t| !state.closed && !*t.cancel.borrow())
            .ok_or(Error::Closed)?;
        t.tx.try_send(event).map_err(|e| {
            t.cancel.send_replace(true);
            match e {
                mpsc::error::TrySendError::Full(_) => Error::Busy,
                _ => Error::Closed,
            }
        })
    }
}
impl ClientInbox {
    pub(crate) fn restore_routes(&mut self, rooms: Vec<RoomHandle>) {
        self.pending
            .extend(rooms.into_iter().map(ClientEvent::Joined));
    }
    pub(crate) fn try_recv(&mut self) -> Option<ClientEvent> {
        if *self.cancel.borrow() {
            None
        } else {
            self.pending.pop_front().or_else(|| self.rx.try_recv().ok())
        }
    }
    async fn receive(&mut self) -> Option<ClientEvent> {
        if *self.cancel.borrow() {
            return None;
        }
        tokio::select! { biased; _ = self.cancel.changed() => None, event = self.rx.recv() => event }
    }
    pub async fn recv(&mut self) -> Option<ClientEvent> {
        if *self.cancel.borrow() {
            return None;
        }
        if let Some(event) = self.pending.pop_front() {
            return Some(event);
        }
        self.receive().await
    }
    /// Discover a route without discarding game frames queued before it.
    /// If preceding frames fill the bounded pending queue, consume them with recv first.
    pub async fn next_room(&mut self) -> Option<RoomHandle> {
        if *self.cancel.borrow() {
            return None;
        }
        if let Some(index) = self
            .pending
            .iter()
            .position(|event| matches!(event, ClientEvent::Joined(_)))
            && let Some(ClientEvent::Joined(room)) = self.pending.remove(index)
        {
            return Some(room);
        }
        while self.pending.len() < self.capacity {
            let event = self.receive().await?;
            if let ClientEvent::Joined(room) = event {
                return Some(room);
            }
            self.pending.push_back(event);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Principal;
    #[tokio::test]
    async fn transport_responses_never_cross_into_a_replacement_generation() {
        let first = ClientHandle::channel(
            Player {
                session_id: uuid::Uuid::new_v4(),
                principal: Principal::guest(),
                name: "guest".into(),
                avatar_url: None,
            },
            8,
            1024,
        );
        let mut second = first.handle.attach(true).unwrap();
        assert_eq!(
            first
                .handle
                .send_current(first.generation, Bytes::from_static(b"stale reply")),
            Err(Error::Closed)
        );
        assert!(second.inbox.try_recv().is_none());
        second
            .handle
            .send_current(second.generation, Bytes::from_static(b"current reply"))
            .unwrap();
        let Some(ClientEvent::Frame(frame)) = second.inbox.recv().await else {
            panic!("missing current reply")
        };
        assert_eq!(&frame[..], b"current reply");
    }
}
