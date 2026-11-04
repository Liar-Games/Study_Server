/** Dependency-free browser transport for the version 1 game protocol. */
const encoder = new TextEncoder();
const decoder = new TextDecoder('utf-8', { fatal: true });
const MAX_FRAME_BYTES = 1024 * 1024;
const MAX_U64 = (1n << 64n) - 1n;

function integer(value, max, label) {
  if (!Number.isSafeInteger(value) || value < 0 || value > max) throw new RangeError(`Invalid ${label}`);
  return value;
}
function bytes(value) {
  if (value instanceof Uint8Array) return value;
  if (value instanceof ArrayBuffer) return new Uint8Array(value);
  throw new TypeError('Expected binary Uint8Array or ArrayBuffer');
}
function roomBigInt(value) {
  if (typeof value === 'number') value = BigInt(integer(value, Number.MAX_SAFE_INTEGER, 'room ID'));
  if (typeof value !== 'bigint' || value < 0n || value > MAX_U64) throw new RangeError('Invalid room ID');
  return value;
}
function jsonRoom(value) {
  const room = roomBigInt(value);
  if (room > BigInt(Number.MAX_SAFE_INTEGER)) throw new RangeError('JSON room ID exceeds the safe integer range');
  return Number(room);
}
function object(value) {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) throw new TypeError('Expected an object');
  return value;
}
function bodyWithRoom(value) {
  const result = { ...object(value) };
  if (Object.hasOwn(result, 'room_id')) result.room_id = jsonRoom(result.room_id);
  return result;
}
function serverError(code) {
  const error = new Error(String(code)); error.code = code; return error;
}

/** Encode the 11-byte big-endian header followed by an opaque payload. */
export function encodeGame(roomId, opcode, payload = new Uint8Array()) {
  const room = roomBigInt(roomId); integer(opcode, 65535, 'opcode');
  if (!(payload instanceof Uint8Array)) throw new TypeError('Expected a Uint8Array payload');
  const frame = new Uint8Array(11 + payload.byteLength);
  const view = new DataView(frame.buffer); frame[0] = 10;
  view.setBigUint64(1, room, false); view.setUint16(9, opcode, false); frame.set(payload, 11);
  return frame;
}

/** Room IDs always decode to bigint; the payload is a view into the input. */
export function decodeGame(input) {
  const frame = bytes(input);
  if (frame.byteLength < 11 || frame[0] !== 10) throw new Error('Invalid GAME frame');
  const view = new DataView(frame.buffer, frame.byteOffset, frame.byteLength);
  return { roomId: view.getBigUint64(1, false), opcode: view.getUint16(9, false), payload: frame.subarray(11) };
}

/** Encode an arbitrary tagged UTF-8 JSON control message. */
export function encodeJSON(tag, value) {
  integer(tag, 255, 'tag');
  const json = JSON.stringify(value);
  if (json === undefined) throw new TypeError('Value is not JSON serializable');
  const body = encoder.encode(json); const frame = new Uint8Array(body.byteLength + 1);
  frame[0] = tag; frame.set(body, 1); return frame;
}
export function decodeJSON(input) {
  const frame = bytes(input);
  if (frame.byteLength < 2 || frame.byteLength > MAX_FRAME_BYTES) throw new Error('Invalid JSON frame size');
  return { tag: frame[0], value: JSON.parse(decoder.decode(frame.subarray(1))) };
}

export class GameClient {
  #WebSocket; #timeoutMs; #socket = null; #session = null;
  #connecting = null; #pending = new Map(); #nextId = 1; #listeners = new Map();

  constructor(url, { WebSocket = globalThis.WebSocket, timeoutMs = 5000 } = {}) {
    if (typeof WebSocket !== 'function') throw new TypeError('A WebSocket constructor is required');
    if (!Number.isFinite(timeoutMs) || timeoutMs <= 0 || timeoutMs > 2147483647) throw new RangeError('Invalid timeoutMs');
    this.url = url; this.#WebSocket = WebSocket; this.#timeoutMs = timeoutMs;
  }
  get session() { return this.#session; }

  /** Register a listener. Exceptions are isolated and reported to listenerError. */
  on(event, listener) {
    if (typeof listener !== 'function') throw new TypeError('Expected a listener function');
    let listeners = this.#listeners.get(event);
    if (!listeners) this.#listeners.set(event, listeners = new Set());
    listeners.add(listener);
    return () => { listeners.delete(listener); if (!listeners.size) this.#listeners.delete(event); };
  }
  #emit(event, value) {
    for (const listener of [...(this.#listeners.get(event) ?? [])]) {
      try {
        const result = listener(value);
        if (result && typeof result.then === 'function') {
          Promise.resolve(result).catch(error => { if (event !== 'listenerError') this.#emit('listenerError', error); });
        }
      }
      catch (error) { if (event !== 'listenerError') this.#emit('listenerError', error); }
    }
  }

  /** Auth is caller supplied on every connection, including manual resume. */
  async connect(auth) {
    if (this.#socket) throw new Error('Client is already connecting or connected');
    let authFrame = encodeJSON(1, auth);
    const socket = new this.#WebSocket(this.url); this.#socket = socket; socket.binaryType = 'arraybuffer';
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        if (this.#socket === socket) this.#disconnect(new Error('Connection timed out'), { code: 1000, reason: 'Connection timed out' }, true);
      }, this.#timeoutMs);
      this.#connecting = { resolve, reject, timer };
      socket.onopen = () => {
        socket.onopen = null;
        if (this.#socket !== socket) return;
        try { socket.send(authFrame); }
        catch (error) { this.#disconnect(error, { code: 1000, reason: 'Send failed' }, true); }
        finally { authFrame = null; }
      };
      socket.onmessage = ({ data }) => {
        if (this.#socket !== socket) return;
        try { this.#receive(data); } catch (error) { this.#emit('error', error); }
      };
      socket.onclose = event => {
        if (this.#socket === socket) this.#disconnect(new Error('Connection closed'), { code: event.code, reason: event.reason });
      };
      socket.onerror = () => {
        if (this.#socket !== socket) return;
        const error = new Error('WebSocket connection error');
        this.#disconnect(error, { code: 1000, reason: 'Connection error' }, true); this.#emit('error', error);
      };
    });
  }

  #disconnect(error, detail, closeSocket = false) {
    const socket = this.#socket;
    if (!socket) return;
    this.#socket = null; this.#session = null;
    socket.onopen = socket.onmessage = socket.onclose = socket.onerror = null;
    if (this.#connecting) {
      const pending = this.#connecting; this.#connecting = null; clearTimeout(pending.timer); pending.reject(error);
    }
    this.#rejectRequests(error);
    if (closeSocket) { try { socket.close(detail.code, detail.reason); } catch { /* Already closed. */ } }
    this.#emit('close', detail);
  }
  #rejectRequests(error) {
    const pending = [...this.#pending.values()]; this.#pending.clear();
    for (const item of pending) { clearTimeout(item.timer); item.reject(error); }
  }
  #ready() {
    if (!this.#socket || this.#socket.readyState !== 1 || !this.#session) throw new Error('Client is not connected');
    return this.#socket;
  }

  async command(operation) { return this.requestRaw(3, operation, 4); }

  /** Extension control requests share engine request IDs but specify their own tags. */
  async requestRaw(tag, body, responseTag) {
    integer(tag, 255, 'tag'); integer(responseTag, 255, 'response tag');
    const socket = this.#ready(); const operation = bodyWithRoom(body);
    if (Object.hasOwn(operation, 'request_id')) throw new TypeError('request_id is assigned by the client');
    let requestId = this.#nextId;
    while (this.#pending.has(requestId)) requestId = requestId === 4294967295 ? 1 : requestId + 1;
    this.#nextId = requestId === 4294967295 ? 1 : requestId + 1;
    const packet = encodeJSON(tag, { ...operation, request_id: requestId });
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.#pending.delete(requestId); reject(new Error(`Request ${requestId} timed out`)); }, this.#timeoutMs);
      this.#pending.set(requestId, { resolve, reject, timer, responseTag });
      try { socket.send(packet); }
      catch (error) { clearTimeout(timer); this.#pending.delete(requestId); reject(error); }
    });
  }

  send(roomId, opcode, payload = new Uint8Array()) { this.#ready().send(encodeGame(roomId, opcode, payload)); }
  sendJSON(roomId, opcode, value) {
    const json = JSON.stringify(value);
    if (json === undefined) throw new TypeError('Value is not JSON serializable');
    this.send(roomId, opcode, encoder.encode(json));
  }
  close(code = 1000, reason = '') {
    integer(code, 4999, 'close code');
    if (code !== 1000 && code < 3000) throw new RangeError('Invalid close code');
    if (typeof reason !== 'string' || encoder.encode(reason).byteLength > 123) throw new RangeError('Invalid close reason');
    this.#disconnect(new Error('Connection closed'), { code, reason }, true);
  }

  #receive(input) {
    const frame = bytes(input);
    if (!frame.byteLength || frame.byteLength > MAX_FRAME_BYTES) throw new Error('Invalid frame size');
    if (frame[0] === 10) { this.#emit('game', decodeGame(frame)); return; }
    const { tag, value } = decodeJSON(frame);
    if (tag === 255) {
      if (typeof value !== 'string') throw new Error('Invalid ERROR frame');
      const error = serverError(value);
      if (this.#connecting) this.#disconnect(error, { code: 1000, reason: 'Authentication failed' }, true);
      else this.#rejectRequests(error);
      this.#emit('error', error); return;
    }
    if (tag === 2) {
      object(value); object(value.player);
      if (value.version !== 1 || typeof value.player.session_id !== 'string' || typeof value.resume_token !== 'string' || typeof value.resumed !== 'boolean') throw new Error('Invalid WELCOME frame');
      jsonRoom(value.global_room);
      if (!this.#connecting) throw new Error('Unexpected WELCOME frame');
      this.#session = value;
      const pending = this.#connecting; this.#connecting = null; clearTimeout(pending.timer); pending.resolve(value);
      this.#emit('welcome', value); return;
    }
    if (tag === 5) { object(value); jsonRoom(value.room_id); this.#emit('roomClosed', value); return; }
    if (tag === 4) {
      object(value);
      if (value.request_id !== null) integer(value.request_id, 4294967295, 'request ID');
      if (value.room_id !== null) jsonRoom(value.room_id);
      if (value.error !== null && typeof value.error !== 'string') throw new Error('Invalid REPLY frame');
      if (value.request_id === null) { this.#emit('matched', value); return; }
    }
    if (value && typeof value === 'object' && value.request_id != null) {
      integer(value.request_id, 4294967295, 'request ID');
      const pending = this.#pending.get(value.request_id);
      if (pending?.responseTag === tag) {
        this.#pending.delete(value.request_id); clearTimeout(pending.timer);
        if (value.error != null) pending.reject(serverError(value.error)); else pending.resolve(value);
      }
    }
    if (tag === 4) this.#emit('reply', value);
    else this.#emit('control', { tag, value });
  }
}
