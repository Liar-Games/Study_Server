//! Game code has no socket, Redis or manager dependency.
use crate::{
    Error, Result,
    actors::client::ClientHandle,
    protocol::GameFrame,
    types::{Player, RoomId, SessionId},
};
use bytes::Bytes;
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

pub enum GameEvent {
    Joined(Player),
    /// Temporary transport loss; membership and game state are retained.
    Disconnected(SessionId),
    /// Restored transport. Send this player the current authoritative snapshot.
    Reconnected(Player),
    /// Final membership removal, including grace expiry.
    Left(SessionId),
    /// Normal room shutdown. Games may enqueue bounded persistence work here.
    Closing,
    Message {
        player: SessionId,
        opcode: u16,
        payload: Bytes,
    },
    /// Result from a trusted asynchronous game worker.
    System {
        opcode: u16,
        payload: Bytes,
    },
    Tick,
}
/// Callbacks execute on the room actor. Never block or perform I/O in a callback.
/// Async workers may return results through RoomHandle::event.
pub trait GameLogic: Send + 'static {
    fn admit(&self, _player: &Player) -> bool {
        true
    }
    fn tick_interval(&self) -> Option<Duration> {
        None
    }
    fn on_event(&mut self, event: GameEvent, ctx: &mut RoomContext<'_>);
}

pub struct RoomContext<'a> {
    pub(crate) handle: &'a crate::actors::room::RoomHandle,
    pub(crate) id: RoomId,
    pub(crate) host: Option<SessionId>,
    pub(crate) members: &'a HashMap<SessionId, ClientHandle>,
    pub(crate) closing: &'a mut bool,
}
impl RoomContext<'_> {
    pub fn handle(&self) -> crate::actors::room::RoomHandle {
        self.handle.clone()
    }
    pub fn room_id(&self) -> RoomId {
        self.id
    }
    pub fn host(&self) -> Option<SessionId> {
        self.host
    }
    pub fn players(&self) -> impl Iterator<Item = &Player> {
        self.members.values().map(ClientHandle::player)
    }
    pub fn close(&mut self) {
        if self.id != 0 {
            *self.closing = true;
        }
    }
    pub fn send(&self, player: SessionId, opcode: u16, payload: Bytes) -> Result<()> {
        let client = self.members.get(&player).ok_or(Error::NotMember)?;
        client.send(
            GameFrame {
                room_id: self.id,
                opcode,
                payload,
            }
            .encode(),
        )
    }
    /// Serialize a game payload once, preserving room membership and frame limits.
    pub fn send_json<T: serde::Serialize + ?Sized>(
        &self,
        player: SessionId,
        opcode: u16,
        payload: &T,
    ) -> Result<()> {
        let payload = serde_json::to_vec(payload).map_err(|_| Error::InvalidPacket)?;
        self.send(player, opcode, Bytes::from(payload))
    }
    /// Serialize once and share the encoded frame among this room's members.
    pub fn broadcast_json<T: serde::Serialize + ?Sized>(
        &self,
        opcode: u16,
        payload: &T,
    ) -> Result<usize> {
        let payload = serde_json::to_vec(payload).map_err(|_| Error::InvalidPacket)?;
        Ok(self.broadcast(opcode, Bytes::from(payload)))
    }
    /// Returns the number enqueued, not a network-delivery acknowledgement.
    pub fn send_many(
        &self,
        players: impl IntoIterator<Item = SessionId>,
        opcode: u16,
        payload: Bytes,
    ) -> usize {
        let frame = GameFrame {
            room_id: self.id,
            opcode,
            payload,
        }
        .encode();
        let mut seen = HashSet::new();
        players
            .into_iter()
            .filter(|id| seen.insert(*id))
            .filter_map(|id| self.members.get(&id))
            .filter(|client| client.send(frame.clone()).is_ok())
            .count()
    }
    pub fn broadcast(&self, opcode: u16, payload: Bytes) -> usize {
        self.fanout(None, opcode, payload)
    }
    pub fn broadcast_except(&self, player: SessionId, opcode: u16, payload: Bytes) -> usize {
        self.fanout(Some(player), opcode, payload)
    }
    fn fanout(&self, exclude: Option<SessionId>, opcode: u16, payload: Bytes) -> usize {
        let frame = GameFrame {
            room_id: self.id,
            opcode,
            payload,
        }
        .encode();
        self.members
            .iter()
            .filter(|(id, _)| Some(**id) != exclude)
            .filter(|(_, client)| client.send(frame.clone()).is_ok())
            .count()
    }
}

/// Small default/example logic: opcode 1 echoes, 2 broadcasts to other members.
#[derive(Default)]
pub struct EchoGame;
impl GameLogic for EchoGame {
    fn on_event(&mut self, event: GameEvent, ctx: &mut RoomContext<'_>) {
        if let GameEvent::Message {
            player,
            opcode,
            payload,
        } = event
        {
            match opcode {
                1 => {
                    let _ = ctx.send(player, opcode, payload);
                }
                2 => {
                    ctx.broadcast_except(player, opcode, payload);
                }
                _ => {}
            }
        }
    }
}
