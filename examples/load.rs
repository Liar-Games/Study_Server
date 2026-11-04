//! Real WebSocket load generator. Run against an isolated release server.
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use study_server::protocol::{self, GameFrame};
use tokio::{net::TcpStream, sync::Barrier};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;
type AnyError = Box<dyn std::error::Error + Send + Sync>;

async fn read(ws: &mut Ws) -> Result<Vec<u8>, AnyError> {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await?
            .ok_or("socket closed")??
        {
            Message::Binary(data) => return Ok(data),
            Message::Ping(data) => ws.send(Message::Pong(data)).await?,
            message => return Err(format!("unexpected frame {message:?}").into()),
        }
    }
}
async fn control(ws: &mut Ws, body: serde_json::Value) -> Result<u64, AnyError> {
    ws.send(Message::Binary(
        protocol::json(protocol::COMMAND, &body).to_vec(),
    ))
    .await?;
    let packet = read(ws).await?;
    if packet[0] != protocol::REPLY {
        return Err(format!("control response: {packet:?}").into());
    }
    let reply: serde_json::Value = serde_json::from_slice(&packet[1..])?;
    if !reply["error"].is_null() {
        return Err(format!("control failure: {reply}").into());
    }
    Ok(reply["room_id"].as_u64().unwrap_or_default())
}
#[tokio::main]
async fn main() -> Result<(), AnyError> {
    let args: Vec<_> = std::env::args().collect();
    let url = args
        .get(1)
        .map(String::as_str)
        .unwrap_or("ws://127.0.0.1:8080/ws");
    let n: usize = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(64);
    let rounds: usize = args.get(3).map(|s| s.parse()).transpose()?.unwrap_or(200);
    let size: usize = args.get(4).map(|s| s.parse()).transpose()?.unwrap_or(64);
    let mode = args.get(5).map(String::as_str).unwrap_or("echo");
    let pipeline: usize = args.get(6).map(|s| s.parse()).transpose()?.unwrap_or(4);
    let group: usize = args.get(7).map(|s| s.parse()).transpose()?.unwrap_or(8);
    if n == 0
        || rounds == 0
        || size < 16
        || pipeline == 0
        || group < 2
        || !n.is_multiple_of(group)
        || !["echo", "broadcast"].contains(&mode)
    {
        return Err("invalid load parameters".into());
    }
    let broadcast = mode == "broadcast";
    let mut sockets = Vec::with_capacity(n);
    let mut room = 0;
    for i in 0..n {
        let (mut ws, _) = connect_async(url).await?;
        if let MaybeTlsStream::Plain(stream) = ws.get_ref() {
            stream.set_nodelay(true)?;
        }
        ws.send(Message::Binary(
            protocol::json(
                protocol::AUTH,
                &serde_json::json!({"mode":"guest","version":1}),
            )
            .to_vec(),
        ))
        .await?;
        if read(&mut ws).await?[0] != protocol::WELCOME {
            return Err("authentication failed".into());
        }
        if i % group == 0 {
            room = control(
                &mut ws,
                serde_json::json!({"request_id":1,"op":"create","capacity":group}),
            )
            .await?;
        } else {
            control(
                &mut ws,
                serde_json::json!({"request_id":1,"op":"join","room_id":room}),
            )
            .await?;
        }
        sockets.push((i, ws, room));
    }
    let base = Instant::now();
    let barrier = Arc::new(Barrier::new(n + 1));
    let finished = Arc::new(Barrier::new(n));
    let mut tasks = tokio::task::JoinSet::new();
    for (i, mut ws, room) in sockets {
        let barrier = barrier.clone();
        let finished = finished.clone();
        tasks.spawn(async move {
            let mut samples = Vec::new();
            let mut sequence = 0usize;
            let mut expected = vec![0u32; group];
            barrier.wait().await;
            while sequence < rounds {
                let batch = if broadcast {
                    1
                } else {
                    pipeline.min(rounds - sequence)
                };
                for offset in 0..batch {
                    let mut payload = vec![0x5a; size];
                    payload[..8].copy_from_slice(&(base.elapsed().as_nanos() as u64).to_be_bytes());
                    payload[8..12].copy_from_slice(&(i as u32).to_be_bytes());
                    payload[12..16].copy_from_slice(&((sequence + offset) as u32).to_be_bytes());
                    ws.feed(Message::Binary(
                        GameFrame {
                            room_id: room,
                            opcode: if broadcast { 2 } else { 1 },
                            payload: payload.into(),
                        }
                        .encode()
                        .to_vec(),
                    ))
                    .await?;
                }
                ws.flush().await?;
                let received = if broadcast { group - 1 } else { batch };
                for _ in 0..received {
                    let wire = read(&mut ws).await.map_err(|error| {
                        format!("client={i} sent={sequence} expected={expected:?}: {error}")
                    })?;
                    let frame = GameFrame::decode(Bytes::from(wire))?;
                    if frame.room_id != room
                        || frame.payload.len() != size
                        || frame.opcode != if broadcast { 2 } else { 1 }
                    {
                        return Err::<Vec<f64>, AnyError>("incorrect game frame".into());
                    }
                    let stamp = u64::from_be_bytes(frame.payload[..8].try_into()?);
                    let sender = u32::from_be_bytes(frame.payload[8..12].try_into()?) as usize;
                    let seq = u32::from_be_bytes(frame.payload[12..16].try_into()?);
                    if sender / group != i / group
                        || (broadcast && sender == i)
                        || (!broadcast && sender != i)
                        || seq != expected[sender % group]
                    {
                        return Err("incorrect sender or sequence".into());
                    }
                    expected[sender % group] += 1;
                    samples.push((base.elapsed().as_nanos() as u64 - stamp) as f64 / 1000.0);
                }
                sequence += batch;
            }
            // Keep all senders connected until every recipient has observed its final packet.
            // Closing earlier can invalidate a last inbound packet still waiting in the room mailbox.
            finished.wait().await;
            let _ = ws.close(None).await;
            Ok::<_, AnyError>(samples)
        });
    }
    barrier.wait().await;
    let started = Instant::now();
    let mut samples = Vec::new();
    while let Some(result) = tasks.join_next().await {
        samples.extend(result??);
    }
    let seconds = started.elapsed().as_secs_f64();
    samples.sort_unstable_by(f64::total_cmp);
    let percentile = |p: f64| samples[((samples.len() - 1) as f64 * p).round() as usize];
    println!(
        "{}",
        serde_json::json!({"mode":mode,"clients":n,"rooms":n/group,"payload_bytes":size,"rounds":rounds,"pipeline":if broadcast {1}else{pipeline},"delivered":samples.len(),"seconds":seconds,"deliveries_per_second":samples.len() as f64/seconds,"latency_us":{"p50":percentile(0.50),"p95":percentile(0.95),"p99":percentile(0.99),"max":samples.last()},"errors":0})
    );
    Ok(())
}
