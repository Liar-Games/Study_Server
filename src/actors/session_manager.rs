use crate::{
    Error, Result,
    actors::client::{ClientHandle, ConnectedClient},
    types::{Identity, Principal, SessionId},
};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;

enum Command {
    Register(ClientHandle, String, oneshot::Sender<Result<()>>),
    Suspend(SessionId, u64, oneshot::Sender<bool>),
    Resume(
        SessionId,
        String,
        Option<Identity>,
        oneshot::Sender<Result<ConnectedClient>>,
    ),
    Expire(oneshot::Sender<(Vec<SessionId>, Vec<SessionId>)>),
    Remove(SessionId, oneshot::Sender<()>),
    Count(oneshot::Sender<usize>),
    Online(Vec<uuid::Uuid>, oneshot::Sender<Vec<bool>>),
    Resolve(Vec<SessionId>, oneshot::Sender<Vec<ClientHandle>>),
}
#[derive(Clone)]
pub struct SessionManager {
    tx: mpsc::Sender<Command>,
}
impl SessionManager {
    pub(crate) fn start(
        capacity: usize,
        grace: Duration,
        mut stop: watch::Receiver<bool>,
    ) -> (Self, tokio::task::JoinHandle<()>) {
        let (tx, mut rx) = mpsc::channel(128);
        let task = tokio::spawn(async move {
            let mut sessions = HashMap::<SessionId, ClientHandle>::new();
            let mut proofs = HashMap::<SessionId, String>::new();
            let mut suspended = HashMap::<SessionId, (u64, Instant)>::new();
            let mut accounts = HashMap::<uuid::Uuid, ClientHandle>::new();
            loop {
                tokio::select! {
                    biased;
                    _ = stop.changed() => break,
                    cmd = rx.recv() => match cmd {
                        Some(Command::Register(client, proof, reply)) => {
                            let account = match client.player().principal { Principal::Account(id) => Some(id), _ => None };
                            let result = if sessions.len() >= capacity { Err(Error::Limit) }
                            else if account.is_some_and(|id| accounts.contains_key(&id)) { Err(Error::AlreadyConnected) }
                            else if client.is_closed() { Err(Error::Closed) }
                            else {
                                if let Some(id) = account { accounts.insert(id, client.clone()); }
                                proofs.insert(client.id(), proof);
                                sessions.insert(client.id(), client.clone());
                                Ok(())
                            };
                            if reply.send(result).is_err() && sessions.remove(&client.id()).is_some() {
                                if let Some(id) = account { accounts.remove(&id); }
                                proofs.remove(&client.id());
                                client.close_final();
                            }
                        },
                        Some(Command::Suspend(id, generation, reply)) => {
                            let changed = sessions.get(&id).is_some_and(|client| client.suspend(generation));
                            if changed { suspended.insert(id, (generation, Instant::now() + grace)); }
                            let _ = reply.send(changed);
                        },
                        Some(Command::Resume(id, proof, identity, reply)) => {
                            let result = (|| {
                                let client = sessions.get(&id).ok_or(Error::AuthFailed)?;
                                if suspended.get(&id).is_some_and(|(_, deadline)| *deadline <= Instant::now()) || proofs.get(&id) != Some(&proof) { return Err(Error::AuthFailed); }
                                match (&client.player().principal, identity.as_ref().map(|i| &i.principal)) {
                                    (Principal::Account(account), Some(Principal::Account(fresh))) if account == fresh => {},
                                    (Principal::Guest(_), None) => {},
                                    _ => return Err(Error::AuthFailed),
                                }
                                let connected = client.attach(true)?;
                                proofs.insert(id, connected.resume_token.clone());
                                suspended.remove(&id);
                                Ok(connected)
                            })();
                            if let Err(Ok(connected)) = reply.send(result) { connected.handle.suspend(connected.generation); suspended.insert(id, (connected.generation, Instant::now() + grace)); }
                        },
                        Some(Command::Expire(reply)) => {
                            let now = Instant::now();
                            let mut disconnected = Vec::new();
                            for (id, client) in &sessions {
                                if let Some(generation) = client.suspend_closed() {
                                    suspended.insert(*id, (generation, now + grace));
                                    disconnected.push(*id);
                                }
                            }
                            let expired: Vec<_> = suspended.iter().filter(|(_, (_, deadline))| *deadline <= now).map(|(id, _)| *id).collect();
                            for id in &expired {
                                suspended.remove(id); proofs.remove(id);
                                if let Some(client) = sessions.remove(id) {
                                    client.close_final();
                                    if let Principal::Account(account) = client.player().principal { accounts.remove(&account); }
                                }
                            }
                            let _ = reply.send((disconnected, expired));
                        },
                        Some(Command::Remove(id, reply)) => {
                            proofs.remove(&id); suspended.remove(&id);
                            if let Some(client) = sessions.remove(&id) {
                                client.close_final();
                                if let Principal::Account(id) = client.player().principal { accounts.remove(&id); }
                            }
                            let _ = reply.send(());
                        },
                        Some(Command::Online(ids, reply)) => {
                            let _ = reply.send(ids.iter().map(|id| accounts.get(id).is_some_and(|client| !client.is_closed())).collect());
                        },
                        Some(Command::Count(reply)) => { let _ = reply.send(sessions.len()); },
                        Some(Command::Resolve(ids, reply)) => {
                            let mut seen = HashSet::new();
                            let found = ids.into_iter().filter(|id| seen.insert(*id)).filter_map(|id| sessions.get(&id).cloned()).collect();
                            let _ = reply.send(found);
                        },
                        None => break,
                    }
                }
            }
            for client in sessions.values() {
                client.close_final();
            }
        });
        (Self { tx }, task)
    }
    pub(crate) async fn register(&self, client: ClientHandle, proof: String) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::Register(client, proof, tx))
            .await
            .map_err(|_| Error::Unavailable)?;
        rx.await.map_err(|_| Error::Unavailable)?
    }
    pub(crate) async fn suspend(&self, id: SessionId, generation: u64) -> bool {
        let (tx, rx) = oneshot::channel();
        if self
            .tx
            .send(Command::Suspend(id, generation, tx))
            .await
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }
    pub(crate) async fn expire(&self) -> (Vec<SessionId>, Vec<SessionId>) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Command::Expire(tx)).await.is_err() {
            return (vec![], vec![]);
        }
        rx.await.unwrap_or_default()
    }
    pub(crate) async fn resume(
        &self,
        id: SessionId,
        token: &str,
        identity: Option<Identity>,
    ) -> Result<ConnectedClient> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::Resume(id, token.into(), identity, tx))
            .await
            .map_err(|_| Error::Unavailable)?;
        rx.await.map_err(|_| Error::Unavailable)?
    }
    pub(crate) async fn remove(&self, id: SessionId) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Command::Remove(id, tx)).await.is_ok() {
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
    /// Batch account presence for this game process only, in input order.
    pub async fn online(&self, ids: Vec<uuid::Uuid>) -> Result<Vec<bool>> {
        if ids.len() > 4096 {
            return Err(Error::Limit);
        }
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::Online(ids, tx))
            .await
            .map_err(|_| Error::Unavailable)?;
        rx.await.map_err(|_| Error::Unavailable)
    }
    /// Trusted server-side M:N delivery. Resolve once, then reuse the handles.
    pub async fn resolve(&self, ids: Vec<SessionId>) -> Result<Vec<ClientHandle>> {
        if ids.len() > 4096 {
            return Err(Error::Limit);
        }
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::Resolve(ids, tx))
            .await
            .map_err(|_| Error::Unavailable)?;
        rx.await.map_err(|_| Error::Unavailable)
    }
}
