import test from 'node:test';
import assert from 'node:assert/strict';
import { GameClient, encodeGame, decodeGame, encodeJSON, decodeJSON } from '../client/index.mjs';

// A controlled network boundary; the SDK and packet encoders are real.
class Socket {
  static instances = [];
  constructor(url) { this.url = url; this.readyState = 0; this.sent = []; Socket.instances.push(this); }
  send(bytes) { if (this.readyState !== 1) throw new Error('Socket closed'); this.sent.push(bytes.slice()); }
  open() { this.readyState = 1; this.onopen?.({}); }
  receive(bytes) { this.onmessage?.({ data: bytes }); }
  close(code = 1000, reason = '') { this.readyState = 3; this.onclose?.({ code, reason }); }
}
const welcome = { version: 1, player: { session_id: 'session', principal: { kind: 'guest', id: 'guest' }, name: 'Guest', avatar_url: null }, global_room: 0, resume_token: 'new-token', resumed: false };
function frame(tag, value) { const body = new TextEncoder().encode(JSON.stringify(value)); return Uint8Array.of(tag, ...body); }
function unpack(bytes) { return JSON.parse(new TextDecoder().decode(bytes.subarray(1))); }
async function connected(options = {}) {
  const client = new GameClient('ws://engine.test', { WebSocket: Socket, ...options });
  const pending = client.connect({ mode: 'guest', version: 1 });
  const socket = Socket.instances.at(-1); socket.open(); socket.receive(frame(2, welcome));
  await pending; return { client, socket };
}

test('GAME codec preserves the golden header and all u64 room bits', () => {
  assert.deepEqual([...encodeGame(42n, 7, Uint8Array.of(9, 8))], [10, 0, 0, 0, 0, 0, 0, 0, 42, 0, 7, 9, 8]);
  const value = decodeGame(encodeGame(18446744073709551615n, 65535, Uint8Array.of(4)));
  assert.deepEqual(value, { roomId: 18446744073709551615n, opcode: 65535, payload: Uint8Array.of(4) });
  const backing = Uint8Array.of(55, 10, 0, 0, 0, 0, 0, 0, 0, 42, 0, 7, 9, 88);
  assert.deepEqual(decodeGame(backing.subarray(1, -1)), { roomId: 42n, opcode: 7, payload: Uint8Array.of(9) });
});

test('codecs reject truncated frames, invalid room IDs, opcodes and payloads', () => {
  for (const room of [-1n, 18446744073709551616n, -1, 1.5, Number.MAX_SAFE_INTEGER + 1, '42', NaN]) assert.throws(() => encodeGame(room, 1, new Uint8Array()));
  for (const opcode of [-1, 65536, 1.5]) assert.throws(() => encodeGame(0n, opcode, new Uint8Array()));
  assert.throws(() => encodeGame(0n, 1, [1]));
  assert.throws(() => decodeGame(Uint8Array.of(10, 0)));
  assert.throws(() => decodeGame(Uint8Array.of(9, ...new Uint8Array(10))));
  assert.throws(() => decodeGame('text'));
});

test('JSON helpers preserve UTF-8 control bodies and reject invalid encodings', () => {
  const bytes = encodeJSON(40, { message: '안녕' });
  assert.equal(bytes[0], 40); assert.deepEqual(decodeJSON(bytes), { tag: 40, value: { message: '안녕' } });
  assert.throws(() => encodeJSON(256, {})); assert.throws(() => decodeJSON(Uint8Array.of(4, 255)));
});

test('connect waits for WELCOME and exposes fresh resume credentials', async () => {
  const client = new GameClient('ws://engine.test', { WebSocket: Socket });
  let observed; client.on('welcome', value => { observed = value; });
  const auth = { mode: 'resume', version: 1, session_id: 'session', token: 'old-token', ticket: 'fresh' };
  const pending = client.connect(auth); const socket = Socket.instances.at(-1);
  assert.equal(socket.binaryType, 'arraybuffer'); assert.equal(socket.sent.length, 0);
  socket.open(); assert.equal(socket.sent[0][0], 1); assert.deepEqual(unpack(socket.sent[0]), auth);
  const resumed = { ...welcome, resumed: true }; socket.receive(frame(2, resumed));
  assert.deepEqual(await pending, resumed); assert.deepEqual(client.session, resumed); assert.deepEqual(observed, resumed);
  client.close();
});

test('commands correlate out-of-order replies without confusing matched notifications', async () => {
  const { client, socket } = await connected(); let matched;
  client.on('matched', value => { matched = value; });
  const first = client.command({ op: 'join', room_id: 42n }); const second = client.command({ op: 'queue' });
  assert.deepEqual(unpack(socket.sent[1]), { op: 'join', room_id: 42, request_id: 1 });
  assert.deepEqual(unpack(socket.sent[2]), { op: 'queue', request_id: 2 });
  socket.receive(frame(4, { request_id: null, room_id: 43, error: null }));
  socket.receive(frame(4, { request_id: 2, room_id: null, error: null }));
  assert.deepEqual(await second, { request_id: 2, room_id: null, error: null });
  assert.deepEqual(matched, { request_id: null, room_id: 43, error: null });
  socket.receive(frame(4, { request_id: 1, room_id: 42, error: null })); assert.equal((await first).room_id, 42);
  client.close();
});

test('requestRaw uses separate response tags and the shared request ID sequence', async () => {
  const { client, socket } = await connected();
  const pending = client.requestRaw(40, { op: 'application_action' }, 41);
  assert.deepEqual([...socket.sent[1].subarray(0, 1)], [40]); assert.equal(unpack(socket.sent[1]).request_id, 1);
  socket.receive(frame(4, { request_id: 1, room_id: 12, error: null }));
  socket.receive(frame(41, { request_id: 1, result: ['ok'], error: null }));
  assert.deepEqual(await pending, { request_id: 1, result: ['ok'], error: null }); client.close();
});

test('timeouts reject once and late replies remain ordinary events', async () => {
  const { client, socket } = await connected({ timeoutMs: 10 }); let reply;
  const pending = client.command({ op: 'queue' }); await assert.rejects(pending, /timed out/i);
  client.on('reply', value => { reply = value; }); socket.receive(frame(4, { request_id: 1, room_id: 2, error: null }));
  assert.equal(reply.room_id, 2); client.close();
});

test('server errors reject requests and authentication without fallback', async () => {
  const { client, socket } = await connected();
  const refused = client.command({ op: 'join', room_id: 2 });
  const rejection = assert.rejects(refused, /room_full/); socket.receive(frame(4, { request_id: 1, room_id: null, error: 'room_full' })); await rejection;
  const a = client.command({ op: 'queue' }); const b = client.command({ op: 'cancel_queue' });
  const failures = Promise.all([assert.rejects(a, /invalid_packet/), assert.rejects(b, /invalid_packet/)]);
  socket.receive(frame(255, 'invalid_packet')); await failures; client.close();
  const fresh = new GameClient('ws://engine.test', { WebSocket: Socket }); const auth = fresh.connect({ mode: 'ticket', version: 1, ticket: 'bad' });
  const authFailure = assert.rejects(auth, /auth_failed/); const authSocket = Socket.instances.at(-1); authSocket.open(); authSocket.receive(frame(255, 'auth_failed')); await authFailure;
  assert.equal(authSocket.readyState, 3);
});

test('close rejects pending work, clears session and permits manual reconnect', async () => {
  const { client, socket } = await connected({ timeoutMs: 20 });
  let close; client.on('close', value => { close = value; });
  const pending = client.command({ op: 'queue' }); const rejection = assert.rejects(pending, /closed/i);
  socket.close(1006, 'lost'); await rejection; assert.equal(client.session, null); assert.equal(close.code, 1006);
  const again = client.connect({ mode: 'resume', version: 1, session_id: 'session', token: 'new-token' });
  const next = Socket.instances.at(-1); next.open(); next.receive(frame(2, { ...welcome, resumed: true }));
  assert.equal((await again).resumed, true); client.close();
});

test('binary and JSON send helpers use exact GAME frames, with distinct room closure events', async () => {
  const { client, socket } = await connected(); let game; let closed;
  client.on('game', value => { game = value; }); client.on('roomClosed', value => { closed = value; });
  client.send(42n, 7, Uint8Array.of(9, 8)); assert.deepEqual([...socket.sent[1]], [10, 0, 0, 0, 0, 0, 0, 0, 42, 0, 7, 9, 8]);
  client.sendJSON(42, 8, { x: 1 }); assert.equal(new TextDecoder().decode(socket.sent[2].subarray(11)), '{"x":1}');
  socket.receive(socket.sent[1]); assert.equal(game.roomId, 42n);
  socket.receive(frame(5, { room_id: 42 })); assert.deepEqual(closed, { room_id: 42 }); client.close();
});

test('malformed frames and throwing callbacks cannot break later transport cleanup', async () => {
  const { client, socket } = await connected(); const errors = []; const callbacks = [];
  client.on('error', error => { errors.push(error); throw new Error('observer failed'); });
  client.on('listenerError', error => { callbacks.push(error); });
  client.on('game', () => { throw new Error('game callback'); });
  for (const bytes of [new Uint8Array(), Uint8Array.of(10), Uint8Array.of(4, 255), 'text', frame(4, { request_id: 1, room_id: 9007199254740992, error: null }), new Uint8Array(1024 * 1024 + 1)]) assert.doesNotThrow(() => socket.receive(bytes));
  assert.equal(errors.length, 6); socket.receive(encodeGame(0n, 1, new Uint8Array())); assert.equal(callbacks.length, 7);
  const pending = client.command({ op: 'queue' }); const rejected = assert.rejects(pending, /closed/i); socket.close(); await rejected;
});

test('room command validation never loses bigint precision or accepts caller request IDs', async () => {
  const { client, socket } = await connected();
  for (const room_id of [9007199254740992n, 9007199254740992, -1, 1.5, '42']) await assert.rejects(client.command({ op: 'join', room_id }));
  await assert.rejects(client.command({ op: 'queue', request_id: 999 }));
  assert.equal(socket.sent.length, 1); client.close();
});

test('connect timeout and pre-auth close settle the connect promise', async () => {
  const slow = new GameClient('ws://engine.test', { WebSocket: Socket, timeoutMs: 10 });
  await assert.rejects(slow.connect({ mode: 'guest', version: 1 }), /timed out/i); assert.equal(Socket.instances.at(-1).readyState, 3);
  const early = new GameClient('ws://engine.test', { WebSocket: Socket }); const pending = early.connect({ mode: 'guest', version: 1 });
  const rejected = assert.rejects(pending, /closed/i); Socket.instances.at(-1).close(); await rejected;
});

test('async listener failures are reported without unhandled rejections', async () => {
  const { client, socket } = await connected(); const errors = [];
  client.on('game', async () => { throw new Error('async observer'); });
  client.on('listenerError', error => { errors.push(error); });
  socket.receive(encodeGame(0n, 1));
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(errors.length, 1); assert.match(errors[0].message, /async observer/); client.close();
});

test('network errors settle authentication and command requests immediately', async () => {
  const { client, socket } = await connected(); const pending = client.command({ op: 'queue' });
  const rejected = assert.rejects(pending, /connection error/i); socket.onerror({}); await rejected;
  assert.equal(client.session, null); assert.equal(socket.readyState, 3);
  const fresh = new GameClient('ws://engine.test', { WebSocket: Socket });
  const auth = fresh.connect({ mode: 'guest', version: 1 }); const authFailure = assert.rejects(auth, /connection error/i);
  Socket.instances.at(-1).onerror({}); await authFailure;
});

test('failed sends release their request and unsubscribe stops event delivery', async () => {
  const { client, socket } = await connected(); let count = 0;
  const off = client.on('roomClosed', () => { count++; }); off(); socket.receive(frame(5, { room_id: 1 })); assert.equal(count, 0);
  const send = socket.send; socket.send = () => { throw new Error('send failure'); };
  await assert.rejects(client.command({ op: 'queue' }), /send failure/); socket.send = send;
  const pending = client.command({ op: 'queue' }); socket.receive(frame(4, { request_id: 2, room_id: null, error: null })); await pending; client.close();
});
