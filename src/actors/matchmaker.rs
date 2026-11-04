//! Optional FIFO matching. One entry per session, O(log n) insertion/removal.
use crate::{
    Error, Result,
    actors::{client::ClientHandle, room_manager::RoomManager},
    protocol::{self, Reply},
    types::SessionId,
};
use std::collections::{BTreeMap, HashMap};
use tokio::sync::{mpsc, oneshot, watch};

enum Command {
    Join(ClientHandle, oneshot::Sender<Result<()>>),
    Leave(SessionId, oneshot::Sender<()>),
    Count(oneshot::Sender<usize>),
}
#[derive(Clone)]
pub struct Matchmaker {
    tx: mpsc::Sender<Command>,
}
impl Matchmaker {
    pub(crate) fn start(
        rooms: RoomManager,
        size: usize,
        capacity: usize,
        mut stop: watch::Receiver<bool>,
    ) -> (Self, tokio::task::JoinHandle<()>) {
        let (tx, mut rx) = mpsc::channel(128);
        let task = tokio::spawn(async move {
            let mut queue = BTreeMap::<u64, ClientHandle>::new();
            let mut index = HashMap::new();
            let mut sequence = 0u64;
            loop {
                tokio::select! {
                    biased;
                    _ = stop.changed() => break,
                    cmd = rx.recv() => match cmd {
                        Some(Command::Join(client, reply)) => {
                            let result = if size == 0 { Err(Error::Denied) }
                            else if client.is_closed() { Err(Error::Closed) }
                            else if !rooms.eligible(&client).await.unwrap_or(false) { Err(Error::Denied) }
                            else {
                                match index.entry(client.id()) {
                                    std::collections::hash_map::Entry::Occupied(_) => Err(Error::AlreadyQueued),
                                    std::collections::hash_map::Entry::Vacant(entry) => {
                                        if queue.len() >= capacity { Err(Error::Limit) }
                                        else {
                                            sequence += 1;
                                            entry.insert(sequence);
                                            queue.insert(sequence, client);
                                            Ok(())
                                        }
                                    }
                                }
                            };
                            let _ = reply.send(result);
                        },
                        Some(Command::Leave(id, reply)) => {
                            if let Some(key) = index.remove(&id) { queue.remove(&key); }
                            let _ = reply.send(());
                        },
                        Some(Command::Count(reply)) => { let _ = reply.send(queue.len()); },
                        None => break,
                    }
                }
                // No polling timer: new arrivals and cancellations drive matching.
                while size > 0 && queue.len() >= size {
                    let mut players = Vec::with_capacity(size);
                    while players.len() < size {
                        let Some((key, client)) = queue.pop_first() else {
                            break;
                        };
                        index.remove(&client.id());
                        if rooms.eligible(&client).await.unwrap_or(false) {
                            players.push((key, client));
                        }
                    }
                    if players.len() < size {
                        for (key, client) in players {
                            index.insert(client.id(), key);
                            queue.insert(key, client);
                        }
                        break;
                    }
                    let result = rooms
                        .match_create(players.iter().map(|(_, c)| c.clone()).collect(), size)
                        .await;
                    let result = match result {
                        Ok(id) => Ok(id),
                        Err(failure) if !failure.disqualified.is_empty() => {
                            for (key, client) in players {
                                if !failure.disqualified.contains(&client.id())
                                    && !client.is_closed()
                                {
                                    index.insert(client.id(), key);
                                    queue.insert(key, client);
                                }
                            }
                            continue;
                        }
                        Err(failure) => Err(failure.error),
                    };
                    // Admission or capacity failure is explicit; every player may retry.
                    let frame =
                        protocol::json(protocol::REPLY, &Reply::new(None, result.map(Some)));
                    for (_, client) in players {
                        let _ = client.send(frame.clone());
                    }
                }
            }
        });
        (Self { tx }, task)
    }
    pub(crate) async fn join(&self, client: ClientHandle) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::Join(client, tx))
            .await
            .map_err(|_| Error::Unavailable)?;
        rx.await.map_err(|_| Error::Unavailable)?
    }
    pub(crate) async fn leave(&self, id: SessionId) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::Leave(id, tx))
            .await
            .map_err(|_| Error::Unavailable)?;
        rx.await.map_err(|_| Error::Unavailable)
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
