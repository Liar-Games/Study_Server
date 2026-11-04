# WebSocket 프로토콜 v1

접속: `ws(s)://<게임서버>/ws`. 애플리케이션 메시지는 모두 WebSocket **Binary** 프레임이다. JSON 제어 메시지는 1바이트 tag 뒤에 UTF-8 JSON을 붙인다. 텍스트 프레임과 과거 `[10,opcode,payload]` 형식은 허용하지 않는다.

이 문서의 C→S는 클라이언트에서 서버로, S→C는 서버에서 클라이언트로 보내는 방향이다. **tag**는 인증·룸 제어·게임 입력 중 어떤 종류인지 구분하는 엔진 공통 번호다. **opcode**는 GAME 안에서 이동·공격 같은 게임별 행동을 구분하는 번호이고, **payload**는 그 행동의 실제 데이터다. 게임 개발자는 opcode와 payload를 정의하며 엔진의 공통 헤더는 그대로 사용한다.

아래 JSON 예시는 내용을 읽기 쉽게 표시한 것이다. WebSocket에 JSON 문자열만 보내면 안 되며 해당 tag를 앞에 붙인 바이너리 메시지로 보낸다. 브라우저 SDK를 사용하면 이 인코딩과 요청 번호 관리를 직접 구현하지 않아도 된다.

## tag와 인증

| 값 | 방향 | 내용 |
|---|---|---|
| 1 Auth | C→S, 첫 프레임 | 인증 또는 재접속 요청 |
| 2 Welcome | S→C | version, player, global_room, resume_token, resumed |
| 3 Command | C→S | request_id를 포함한 제어 요청 |
| 4 Reply | S→C | 제어 결과 또는 비동기 매칭 결과 |
| 5 RoomClosed | S→C | `{"room_id":42}` |
| 10 Game | 양방향 | 11바이트 헤더와 게임별 payload |
| 255 Error | S→C | JSON 문자열 오류 코드, 예: `"not_member"` |

기본 5초 안에 Auth를 보내야 한다. 티켓 발급과 검증은 게임 애플리케이션의 인증 어댑터 책임이다. 인증 실패·인증 서비스 장애를 게스트 접속으로 바꾸지 않는다.

기본 서버의 `GuestAuth`로는 `guest` 접속과 해당 게스트의 재접속을 사용할 수 있다. `ticket`과 계정 재접속은 애플리케이션이 계정용 `Authenticator`를 연결했을 때 동작한다. 티켓은 계정임을 증명하는 접속용 값이고, `resume_token`은 이미 만들어진 게임 세션을 복구하는 값으로 역할이 다르다. 인증값은 URL에 넣지 않고 첫 AUTH로 보낸다.

```json
{"mode":"guest","version":1}
{"mode":"ticket","version":1,"ticket":"fresh-ticket"}
```

성공 예:

```json
{"version":1,"player":{"session_id":"<uuid>","principal":{"kind":"guest","id":"<uuid>"},"name":"Guest","avatar_url":null},"global_room":0,"resume_token":"<private-token>","resumed":false}
```

`player`는 인증 시 결정한 프로필 스냅샷이다. `principal.kind`는 `guest` 또는 `account`다. 글로벌 룸 0에는 자동 참가하지만, 클라이언트가 room 0으로 보내는 GAME은 `Config.allow_global_input=false` 기본값에서 `denied`다. 신뢰된 서버 코드의 글로벌 방송은 가능하다.

## 연결 생존과 재접속

서버는 기본 20초마다 WebSocket Ping을 보내고 같은 payload의 Pong을 10초 기다린다. 브라우저는 Pong을 자동 전송한다. 이 연결 생존 확인은 게임 입력 여부와 무관하다.

연결이 끊겨도 논리 `session_id`와 기존 방 참가는 기본 60초간 유지된다. 매칭 대기열에서는 빠진다. 유예 안에 새 WebSocket을 열고 첫 Auth로 다음을 보낸다.

```json
{"mode":"resume","version":1,"session_id":"<previous-session-id>","token":"<latest-WELCOME.resume_token>"}
{"mode":"resume","version":1,"session_id":"<previous-session-id>","token":"<latest-WELCOME.resume_token>","ticket":"fresh-account-ticket"}
```

게스트는 토큰으로 복구한다. 계정은 토큰과 같은 계정임을 증명하는 **새 티켓**을 함께 제출한다. 이미 사용한 티켓을 재사용하지 않는다. 성공한 WELCOME은 같은 `session_id`, `resumed:true`, 새 `resume_token`을 반환한다. 토큰은 매번 회전하므로 최신 값으로 교체한다. 서버가 이전 연결의 종료를 아직 감지하지 못했어도 유효한 증명으로 resume하면 이전 연결을 취소하고 새 연결 세대로 교체한다. 만료된 유예, 잘못된 토큰·계정 증명은 복구되지 않는다.

오프라인 출력 큐와 입력 재전송은 없다. 게임은 `GameEvent::Reconnected`에서 최신 상태 스냅샷을 전송한다. 유예 만료나 신뢰된 서버의 최종 해제는 방 참가를 제거한다. 프로세스 재시작 후에는 새 세션·새 방으로 시작한다.

## 제어 요청과 응답

```json
{"request_id":1,"op":"create","capacity":4}
{"request_id":2,"op":"join","room_id":42}
{"request_id":3,"op":"leave","room_id":42}
{"request_id":4,"op":"queue"}
{"request_id":5,"op":"cancel_queue"}
```

```json
{"request_id":1,"room_id":42,"error":null}
{"request_id":2,"room_id":null,"error":"room_full"}
{"request_id":4,"room_id":null,"error":null}
{"request_id":null,"room_id":43,"error":null}
```

`request_id`는 클라이언트가 정하는 u32다. `null`인 Reply는 비동기 매칭 결과이며 queue 수용 응답과 별개다. 순서에 의존하지 않는다. 엔진은 중복 요청 실행을 막지 않으므로 응답이 없더라도 create 등을 임의 재전송하지 않는다. `allow_create`, `allow_join`, `allow_queue`가 false인 동작은 `denied`다. 취소·퇴장은 별도 요청으로 처리한다.

`create` 성공 시 생성자도 그 방에 참가한다. `join` 성공 시 해당 방에 게임 입력을 보낼 수 있다. `leave`는 지정한 게임방에서 최종 퇴장하지만 WebSocket과 글로벌 참가를 유지한다. `queue` 성공은 대기열 등록이며 매칭 완료가 아니다. 매칭되면 별도 Reply의 `room_id`로 게임방을 알린다. 게임방 ID는 다른 플레이어에게 전달해야 직접 `join`할 수 있으며, 방 목록 조회나 초대 링크 생성은 기본 프로토콜에 없다.

오류 코드에는 `invalid_packet`, `auth_failed`, `unavailable`, `already_connected`, `not_member`, `room_not_found`, `room_full`, `denied`, `limit`, `busy`, `closed`, `already_queued`가 있다. 인증 실패는 Error 뒤 연결 종료, 잘못된 입력은 Error, 전송률·프레임 크기 초과와 송신 지연·Pong timeout은 연결 종료로 처리한다.

WebSocket으로 전환하기 전에는 허용하지 않은 Origin에 HTTP 403, 접속 슬롯 부족이나 엔진 비가용에 HTTP 503을 반환한다. 전환 후 게임 권한 거절은 HTTP 응답이 아니라 `denied` 같은 프로토콜 오류다.

## GAME 패킷

| 오프셋 | 길이 | 형식 |
|---|---|---|
| 0 | 1 | tag = 10 |
| 1 | 8 | room_id, u64 big-endian |
| 9 | 2 | opcode, u16 big-endian |
| 11 | 나머지 | 게임별 payload |

`[10,0,0,0,0,0,0,0,42,0,7,9,8]`은 room 42, opcode 7, payload `[9,8]`이다. 사용자 ID를 프레임에 넣지 않는다. 서버가 인증된 세션과 방 멤버십을 확인한다. 방 ID는 프로세스 수명 동안 증가하고 재사용하지 않는다. 중요한 동작의 완료 확인은 게임별 ACK로 구현한다.

big-endian은 큰 자릿수 바이트부터 저장하는 순서다. payload에는 JSON·고정 길이 숫자·별도 바이너리 형식 중 게임이 정한 것을 넣을 수 있다. GAME 자체에는 `request_id`와 자동 응답이 없으므로, 요청과 결과를 대응시켜야 한다면 payload 안에 게임용 요청 번호를 정의한다. 인증된 참가자의 패킷이라도 게임 규칙상 올바른 행동인지는 `GameLogic`이 검사해야 한다.

기본 EchoGame은 opcode 1을 본인에게 반환하고 2를 자신을 제외한 룸 참가자에게 전달한다. 실제 게임은 이 규칙을 교체한다.

## 브라우저 SDK

[`client/index.mjs`](../client/index.mjs)는 의존성 없는 ES 모듈이며 [`index.d.mts`](../client/index.d.mts)를 제공한다. 파일을 정적 자산으로 제공하거나 번들에 포함한다.

```js
import { GameClient } from './client/index.mjs';

const client = new GameClient('ws://localhost:8080/ws');
let latestWelcome;
client.on('welcome', value => { latestWelcome = value; });
client.on('game', ({ roomId, opcode, payload }) => {
  console.log(roomId, opcode, payload); // roomId는 bigint
});
client.on('matched', reply => console.log('matched', reply.room_id));
client.on('roomClosed', ({ room_id }) => console.log('closed', room_id));
client.on('error', error => console.error(error));

// 게임의 인증 어댑터가 접속마다 인증값을 준비한다.
const auth = { mode: 'guest', version: 1 };
await client.connect(auth);
const reply = await client.command({ op: 'create', capacity: 2 });
client.send(reply.room_id, 1, new Uint8Array());
// JSON을 게임 payload로 쓰는 경우:
// client.sendJSON(reply.room_id, 3, { action: 'ready' });
```

위 코드는 `<script type="module">` 또는 번들러의 모듈 코드에서 실행한다. 서버는 `ALLOWED_ORIGINS`에 이 페이지의 주소를 허용해야 한다. 기본 EchoGame이면 빈 payload가 그대로 돌아온다. 다른 브라우저도 별도 `GameClient`로 인증한 뒤 `command({ op: 'join', room_id: reply.room_id })`를 호출하면 같은 방에 참가한다. 두 번째 브라우저에는 첫 번째가 받은 방 ID를 전달한다. 이후 opcode 2로 전송하면 자신을 제외한 같은 방 참가자가 받는다.

`custom_game` 예제 서버를 사용할 때는 예제의 `Config.allowed_origins`를 페이지 주소에 맞게 설정한다. 이 서버의 opcode 1 응답은 카운터 값이므로 다음과 같이 해석할 수 있다.

```js
client.on('game', ({ roomId, opcode, payload }) => {
  // 글로벌 룸과 게임방의 상태가 모두 올 수 있으므로 방도 구분한다.
  if (roomId === 0n || opcode !== 1 || payload.byteLength !== 8) return;
  const view = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
  console.log('counter', view.getBigUint64(0, false));
});
```

JSON payload를 선택한 게임에서는 `JSON.parse(new TextDecoder().decode(payload))`로 해석한다. 기본 EchoGame과 카운터 예제는 JSON 상태를 보내는 게임이 아니므로 각 게임의 payload 규약에 맞게 처리한다.

`connect()`는 WELCOME까지, `command()`는 같은 request_id의 Reply까지 기다린다. 기본 timeout은 각각 5초이며 오류·연결 종료 시 대기 요청을 reject한다. `on()`은 구독 해제 함수를 반환한다. 이벤트 콜백 실패는 `listenerError`로 전달된다. 연결 종료 후 `client.session`은 null이므로 재접속 값은 위 예제처럼 WELCOME에서 보관한다.

오류가 있는 Reply도 `command()`의 Promise를 reject하므로 `try/catch`로 실패를 처리한다. `send()`는 네트워크 수신이나 게임 처리 완료를 기다리는 Promise를 반환하지 않는다. 매칭형 게임은 `command({ op: 'queue' })`의 응답을 입장 완료로 취급하지 않고, `matched` 이벤트에서 받은 방 ID로 플레이를 시작한다. SDK의 기본 timeout은 서버에서 이미 시작한 작업을 취소한다는 뜻이 아니다.

```js
// 이전 연결이 닫힌 뒤, 유예 시간 안에 호출한다.
const previous = latestWelcome;
const resumeAuth = {
  mode: 'resume', version: 1,
  session_id: previous.player.session_id,
  token: previous.resume_token,
};
// 계정인 경우 인증 어댑터에서 새로 발급한 ticket을 추가한다.
await client.connect(resumeAuth);
```

자동 재접속·티켓 보관·요청 재시도는 하지 않는다. `encodeGame`/`decodeGame`은 u64 room ID를 bigint로 유지한다. 제어 JSON의 room ID에는 안전한 정수만 허용하며 bigint를 넘길 때도 `Number.MAX_SAFE_INTEGER` 초과면 거절한다. `encodeJSON`/`decodeJSON`은 tag와 JSON을 처리하고 `requestRaw(tag, body, responseTag)`는 게임 애플리케이션의 추가 제어 프로토콜에 request_id 상관관계를 제공한다. SDK의 수신 프레임 상한은 1 MiB다.

브라우저 Origin은 `ALLOWED_ORIGINS`에 정확하게 등록한다. 페이지의 http/https와 배포 주소에 맞는 ws/wss URL을 사용하고 TLS 종료는 외부 프록시에서 구성한다.
