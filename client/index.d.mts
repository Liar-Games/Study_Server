/** Binary GAME IDs are u64 bigint. Numbers are accepted only when safe integers. */
export type RoomId = bigint | number;
export type Auth =
  | { mode: 'guest'; version: 1 }
  | { mode: 'ticket'; version: 1; ticket: string }
  | { mode: 'resume'; version: 1; session_id: string; token: string; ticket?: string };
export interface Player {
  session_id: string;
  principal: { kind: 'guest' | 'account'; id: string };
  name: string;
  avatar_url: string | null;
}
export interface Welcome {
  version: 1;
  player: Player;
  /** JSON room IDs must fit Number.MAX_SAFE_INTEGER. */
  global_room: number;
  /** Rotated credential from this successful connection. */
  resume_token: string;
  resumed: boolean;
}
export type Operation =
  | { op: 'create'; capacity: number }
  | { op: 'join' | 'leave'; room_id: RoomId }
  | { op: 'queue' | 'cancel_queue' };
export interface Reply {
  request_id: number | null;
  room_id: number | null;
  error: string | null;
}
export interface GameFrame {
  roomId: bigint;
  opcode: number;
  /** View into the decoded input, without a copy. */
  payload: Uint8Array;
}
export interface ControlFrame<T = unknown> { tag: number; value: T }
export interface ClientEvents {
  welcome: Welcome;
  game: GameFrame;
  matched: Reply;
  roomClosed: { room_id: number };
  /** Correlated replies, unmatched replies, and late replies. */
  reply: Reply;
  /** Non-engine JSON control messages, including extension replies. */
  control: ControlFrame;
  /** Malformed input, server ERROR frames, or transport errors. */
  error: Error;
  close: { code: number; reason: string };
  /** Synchronous and asynchronous listener failures, isolated from transport. */
  listenerError: unknown;
}
/** Minimal browser-compatible interface permits controlled test transports. */
export interface WebSocketLike {
  binaryType: string;
  readonly readyState: number;
  onopen: ((event: any) => unknown) | null;
  onmessage: ((event: any) => unknown) | null;
  onerror: ((event: any) => unknown) | null;
  onclose: ((event: any) => unknown) | null;
  send(data: Uint8Array): void;
  close(code?: number, reason?: string): void;
}
export interface ClientOptions {
  WebSocket?: new (url: string) => WebSocketLike;
  /** Connect and per-request timeout; defaults to 5000 milliseconds. */
  timeoutMs?: number;
}
export function encodeGame(roomId: RoomId, opcode: number, payload?: Uint8Array): Uint8Array;
export function decodeGame(input: Uint8Array | ArrayBuffer): GameFrame;
/** UTF-8 JSON with a one-byte control tag. Throws for non-serializable values. */
export function encodeJSON(tag: number, value: unknown): Uint8Array;
/** Requires valid UTF-8 JSON and a frame at most 1 MiB. */
export function decodeJSON<T = unknown>(input: Uint8Array | ArrayBuffer): ControlFrame<T>;

export class GameClient {
  constructor(url: string, options?: ClientOptions);
  readonly url: string;
  /** Latest successful WELCOME; null before authentication or after disconnect. */
  readonly session: Welcome | null;
  /** Returns an unsubscribe function. Listener failures do not interrupt transport. */
  on<K extends keyof ClientEvents>(event: K, listener: (value: ClientEvents[K]) => unknown): () => void;
  /**
   * Opens and authenticates a connection; resolves only on WELCOME.
   * No automatic reconnect, retry, ticket retention, or authentication downgrade.
   * For manual resume use the last player.session_id and resume_token as token.
   * The caller supplies a freshly acquired ticket when account resume requires it.
   */
  connect(auth: Auth): Promise<Welcome>;
  /** Assigns a u32 request_id. Non-null server errors reject the promise. */
  command(operation: Operation): Promise<Reply>;
  /**
   * Extension JSON request with an assigned request_id and expected response tag.
   * Resolves the complete response body, rejecting non-null response.error.
   * A top-level room_id bigint must fit Number.MAX_SAFE_INTEGER for JSON.
   * Caller-provided request_id is rejected. No request is automatically retried.
   */
  requestRaw<T = Record<string, unknown>>(tag: number, body: Record<string, unknown>, responseTag: number): Promise<T>;
  send(roomId: RoomId, opcode: number, payload?: Uint8Array): void;
  /** JSON-encodes only the opaque GAME payload. */
  sendJSON(roomId: RoomId, opcode: number, value: unknown): void;
  /** Rejects pending work immediately. Close codes follow browser WebSocket rules. */
  close(code?: number, reason?: string): void;
}
