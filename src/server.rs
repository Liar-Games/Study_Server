use crate::{
    Error, Result,
    actors::{
        client::{ClientEvent, ClientHandle, ConnectedClient},
        room::RoomHandle,
        session_manager::SessionManager,
    },
    auth::Authenticator,
    engine::Engine,
    protocol::{self, AuthRequest, GameFrame, Reply, Request, Welcome},
};
use axum::{
    Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use bytes::Bytes;
use futures::{SinkExt, StreamExt, future::BoxFuture};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
    time::{Instant, timeout},
};

/// Optional cold-path application requests. Game packets never pass through this hook.
pub trait Application: Send + Sync + 'static {
    fn request(
        &self,
        client: ClientHandle,
        frame: Bytes,
        sessions: SessionManager,
        max_frame: usize,
    ) -> BoxFuture<'_, Result<Bytes>>;
    fn error(&self, _frame: &[u8], error: Error, _max_frame: usize) -> Bytes {
        protocol::json(protocol::ERROR, &error)
    }
}
#[derive(Clone)]
struct App {
    engine: Engine,
    auth: Arc<dyn Authenticator>,
    connections: Arc<tokio::sync::Semaphore>,
    handshakes: Arc<Semaphore>,
    application: Option<Arc<dyn Application>>,
}
pub fn router(engine: Engine, auth: Arc<dyn Authenticator>) -> Router {
    build_router(engine, auth, None)
}
pub fn router_with_application(
    engine: Engine,
    auth: Arc<dyn Authenticator>,
    application: Arc<dyn Application>,
) -> Router {
    build_router(engine, auth, Some(application))
}
fn build_router(
    engine: Engine,
    auth: Arc<dyn Authenticator>,
    application: Option<Arc<dyn Application>>,
) -> Router {
    // A full logical-session table must still admit a bounded replacement socket.
    let pending = engine.config().max_sessions.min(128);
    let connections = Arc::new(Semaphore::new(
        engine.config().max_sessions.saturating_add(pending),
    ));
    let handshakes = Arc::new(Semaphore::new(pending));
    Router::new()
        .route("/ws", get(upgrade))
        .route("/health/live", get(|| async { "ok" }))
        .route("/health/ready", get(ready))
        .with_state(App {
            engine,
            auth,
            connections,
            handshakes,
            application,
        })
}
async fn ready(State(app): State<App>) -> StatusCode {
    if app.engine.is_running() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}
async fn upgrade(State(app): State<App>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    if !app.engine.is_running() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let origins = &app.engine.config().allowed_origins;
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    if match origin {
        Some(origin) => !origins.iter().any(|allowed| allowed == origin),
        None => !origins.is_empty(),
    } {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(permit) = app.connections.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(handshake) = app.handshakes.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    ws.max_message_size(app.engine.config().max_frame)
        .max_frame_size(app.engine.config().max_frame)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            serve_client(socket, app, handshake).await;
        })
}
enum Authentication {
    Fresh(crate::types::Identity),
    Resume {
        session_id: crate::types::SessionId,
        token: String,
        identity: Option<crate::types::Identity>,
    },
}
async fn authenticate(socket: &mut WebSocket, app: &App) -> Result<Authentication> {
    let Some(Ok(Message::Binary(data))) = socket.next().await else {
        return Err(Error::AuthFailed);
    };
    if data.first() != Some(&protocol::AUTH) || data.len() > 1024 {
        return Err(Error::InvalidPacket);
    }
    let request: AuthRequest =
        serde_json::from_slice(&data[1..]).map_err(|_| Error::InvalidPacket)?;
    if request.version() != 1 {
        return Err(Error::AuthFailed);
    }
    match request {
        AuthRequest::Resume {
            session_id,
            token,
            ticket,
            ..
        } => {
            if !(32..=256).contains(&token.len()) {
                return Err(Error::AuthFailed);
            }
            let identity = match ticket {
                Some(ticket) => Some(
                    app.auth
                        .authenticate(AuthRequest::Ticket { version: 1, ticket })
                        .await?,
                ),
                None => None,
            };
            Ok(Authentication::Resume {
                session_id,
                token,
                identity,
            })
        }
        request => Ok(Authentication::Fresh(app.auth.authenticate(request).await?)),
    }
}
async fn serve_client(mut socket: WebSocket, app: App, handshake: OwnedSemaphorePermit) {
    let authenticated = timeout(
        app.engine.config().auth_timeout,
        authenticate(&mut socket, &app),
    )
    .await
    .unwrap_or(Err(Error::AuthFailed));
    // Do not cancel actor registration half-way through an authentication timeout.
    let connected = match authenticated {
        Ok(Authentication::Fresh(identity)) => app.engine.connect_identity(identity).await,
        Ok(Authentication::Resume {
            session_id,
            token,
            identity,
        }) => app.engine.resume(session_id, &token, identity).await,
        Err(error) => Err(error),
    };
    drop(handshake);
    let client = match connected {
        Ok(client) => client,
        Err(error) => {
            let frame = protocol::json(protocol::ERROR, &error);
            let _ = timeout(
                app.engine.config().write_timeout,
                socket.send(Message::Binary(frame.to_vec())),
            )
            .await;
            let _ = timeout(
                app.engine.config().write_timeout,
                socket.send(Message::Close(None)),
            )
            .await;
            return;
        }
    };
    let id = client.handle.id();
    let generation = client.generation;
    let welcome = protocol::json(
        protocol::WELCOME,
        &Welcome {
            version: 1,
            player: client.handle.player(),
            global_room: 0,
            resume_token: &client.resume_token,
            resumed: client.resumed,
        },
    );
    if matches!(
        timeout(
            app.engine.config().write_timeout,
            socket.send(Message::Binary(welcome.to_vec()))
        )
        .await,
        Ok(Ok(()))
    ) {
        run(socket, &app, client).await;
    }
    app.engine.suspend(id, generation).await;
}
async fn control(
    engine: &Engine,
    client: &ClientHandle,
    generation: u64,
    request: Request,
) -> Reply {
    Reply::new(
        Some(request.request_id),
        engine
            .control_current(client, generation, request.operation)
            .await,
    )
}

fn routing(event: ClientEvent, rooms: &mut HashMap<u64, RoomHandle>) -> Option<Bytes> {
    match event {
        ClientEvent::Joined(room) => {
            rooms.insert(room.id(), room);
            None
        }
        ClientEvent::Left(id) => {
            rooms.remove(&id);
            None
        }
        ClientEvent::Frame(data) => Some(data),
    }
}
async fn run(socket: WebSocket, app: &App, mut client: ConnectedClient) {
    let engine = &app.engine;
    let config = engine.config();
    let generation = client.generation;
    // One bounded cold-path job per socket, shared by commands and application I/O.
    let mut jobs = JoinSet::new();
    let (mut writer, mut reader) = socket.split();
    let mut rooms = HashMap::<u64, RoomHandle>::new();
    // Routing notifications precede snapshots. Preserve every queued frame.
    let mut initial = Vec::new();
    for _ in 0..config
        .outbound_capacity
        .saturating_add(config.max_rooms_per_session)
    {
        let Some(event) = client.inbox.try_recv() else {
            break;
        };
        if let Some(frame) = routing(event, &mut rooms) {
            initial.push(frame);
        }
    }
    if !initial.is_empty() {
        let write = async {
            for frame in initial {
                writer.feed(Message::Binary(frame.to_vec())).await?;
            }
            writer.flush().await
        };
        if !matches!(timeout(config.write_timeout, write).await, Ok(Ok(()))) {
            return;
        }
    }
    let mut window = Instant::now();
    let mut messages = 0u32;
    let mut ping =
        tokio::time::interval_at(Instant::now() + config.ping_interval, config.ping_interval);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let pong_deadline = tokio::time::sleep(config.pong_timeout);
    tokio::pin!(pong_deadline);
    let mut awaiting_pong: Option<Vec<u8>> = None;
    let mut sequence = 0u64;
    loop {
        tokio::select! {
            _=&mut pong_deadline,if awaiting_pong.is_some()=>break,
            _=ping.tick(),if awaiting_pong.is_none()=>{
                sequence=sequence.wrapping_add(1);let payload=sequence.to_be_bytes().to_vec();
                if !matches!(timeout(config.write_timeout,writer.send(Message::Ping(payload.clone()))).await,Ok(Ok(()))){break}
                awaiting_pong=Some(payload);pong_deadline.as_mut().reset(Instant::now()+config.pong_timeout);
            },
            event=client.inbox.recv()=>{
                let Some(mut event)=event else{break};
                let write=async {
                    let mut dirty=false;
                    for n in 0..32 {
                        if let Some(frame)=routing(event,&mut rooms){writer.feed(Message::Binary(frame.to_vec())).await?;dirty=true;}
                        if n==31{break}let Some(next)=client.inbox.try_recv()else{break};event=next;
                    }
                    if dirty{writer.flush().await?;}Ok::<(),axum::Error>(())
                };
                if !matches!(timeout(config.write_timeout,write).await,Ok(Ok(()))){break}
            },
            input=reader.next()=>{
                let Some(Ok(input))=input else{break};
                if window.elapsed()>=Duration::from_secs(1){window=Instant::now();messages=0;}
                messages+=1;if messages>config.messages_per_second{break}
                let result=match input {
                    Message::Binary(data) if data.first()==Some(&protocol::GAME)=>{
                        GameFrame::decode(Bytes::from(data)).and_then(|frame|{
                            if frame.room_id==0&&!config.allow_global_input {
                                // Client-originated global relay is disabled. Trusted game logic
                                // may use RoomContext::broadcast / SessionManager::resolve + send.
                                return Err(Error::Denied);
                            }
                            rooms.get(&frame.room_id).ok_or(Error::NotMember)?.packet_current(client.handle.id(),generation,frame.opcode,frame.payload)
                        })
                    },
                    Message::Binary(data) if data.first()==Some(&protocol::COMMAND)&&data.len()<=1024=>{
                        if !client.handle.is_current(generation) { break; }
                        match serde_json::from_slice::<Request>(&data[1..]) {
                            Err(_)=>Err(Error::InvalidPacket),
                            Ok(request)=>{
                                let _=jobs.try_join_next();
                                if jobs.is_empty(){let engine=engine.clone();let handle=client.handle.clone();
                                    jobs.spawn(async move{if handle.is_current(generation){let reply=control(&engine,&handle,generation,request).await;if handle.is_current(generation){let _=handle.send_current(generation,protocol::json(protocol::REPLY,&reply));}}});Ok(())
                                }else{client.handle.send_current(generation,protocol::json(protocol::REPLY,&Reply::new(Some(request.request_id),Err(Error::Busy))))}
                            }
                        }
                    },
                    Message::Binary(data) if data.len()<=1024=>{
                        if !client.handle.is_current(generation) { break; }
                        if let Some(application)=&app.application {
                            let data=Bytes::from(data);let _=jobs.try_join_next();
                            if jobs.is_empty(){let application=application.clone();let handle=client.handle.clone();let sessions=engine.sessions().clone();let max_frame=config.max_frame;
                                jobs.spawn(async move{if !handle.is_current(generation) { return; } let result=application.request(handle.clone(),data.clone(),sessions,max_frame).await;let frame=result.unwrap_or_else(|error|application.error(&data,error,max_frame));if handle.is_current(generation){let _=handle.send_current(generation,frame);}});Ok(())
                            }else{client.handle.send_current(generation,application.error(&data,Error::Busy,config.max_frame))}
                        }else{Err(Error::InvalidPacket)}
                    },
                    Message::Ping(data)=>{if !matches!(timeout(config.write_timeout,writer.send(Message::Pong(data))).await,Ok(Ok(()))){break}Ok(())},
                    Message::Pong(data)=>{if awaiting_pong.as_ref()==Some(&data){awaiting_pong=None;}Ok(())},
                    Message::Close(_)=>break,
                    _=>Err(Error::InvalidPacket),
                };
                if let Err(error)=result&&client.handle.send_current(generation,protocol::json(protocol::ERROR,&error)).is_err(){break}
            }
        }
    }
    drop(jobs);
    let _ = timeout(config.write_timeout, writer.send(Message::Close(None))).await;
}
