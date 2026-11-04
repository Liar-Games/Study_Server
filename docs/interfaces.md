# 액터 인터페이스와 게임 확장

한 게임 프로세스에는 글로벌 룸 0이 하나 있다. 글로벌 룸도 `GameLogic`을 실행하며 모든 인증된 논리 세션이 참가한다. 게임방에 들어가도 글로벌 참가를 유지하고 임의 퇴장은 허용하지 않는다.

액터·mailbox·세션 같은 용어가 익숙하지 않다면 [구조 안내](architecture.md)를 먼저 읽는다. 이 문서는 서버 내부 Rust API를 설명한다. 브라우저에서 사용하는 API와 바이트 형식은 [클라이언트 프로토콜](protocol.md)에 있다.

## 책임과 흐름

```text
WebSocket ↔ 접속 task ── SessionManager (논리 세션·연결 세대·재접속)
              │       ├─ RoomManager (생성·입장·퇴장·감독)
              │       └─ Matchmaker (선택적 FIFO)
              └─ 승인된 RoomHandle → 룸 task / GameLogic
                                          └─ RoomContext → ClientHandle → WebSocket
```

`server.rs`의 접속 task가 실제 WebSocket을 관리한다. `ClientHandle`은 논리 세션 동안 유지되며 각 연결은 별도 `ClientInbox`와 `generation`을 갖는다. 입력은 승인된 룸 경로로 전달하고 룸에서 현재 연결 세대를 다시 검사한다. 성공 Reply 이후에는 해당 방에 입력할 수 있다.

RoomManager는 룸 참가와 세션별 색인을 관리하고 실제 룸이 정원·입장 정책을 판단한다. Matchmaker는 세션당 하나의 FIFO 대기 항목을 관리한다. 기본 설정은 글로벌과 게임방 하나까지 허용한다. `Config.allow_create`, `allow_join`, `allow_queue`는 기본 true이며 false인 동작은 `Denied`다. `allow_global_input`은 기본 false로 글로벌 룸의 클라이언트 입력을 차단한다. 서버의 글로벌 방송은 계속 사용할 수 있다.

## GameLogic

[`examples/custom_game.rs`](../examples/custom_game.rs)는 카운터 게임의 실행 예제다. `Engine::new(config, |room_id| Box::new(MyGame::default())).await?`로 factory를 주입한다. factory는 글로벌을 포함한 각 방마다 호출되며 room 0에는 별도 로비 로직을 반환해도 된다.

`GameLogic`은 게임 규칙을 담는 Rust trait이다. 게임 구조체의 필드에 해당 방의 점수·턴·캐릭터 상태 등을 보관한다. factory는 새 방의 로직 인스턴스를 만드는 함수이고, `RoomContext`는 콜백이 실행되는 동안 현재 방의 참가자와 전송 기능에 접근하는 창구다. 게임로직이 소켓이나 모든 세션의 공유 상태를 직접 소유할 필요는 없다.

처음 구현할 때는 다음 순서로 진행하면 된다.

1. 예제의 `Counter`처럼 방 상태를 가진 구조체와 `GameLogic::on_event`를 구현한다.
2. 클라이언트 행동과 서버 상태에 사용할 opcode·payload를 정한다. 엔진은 게임 payload의 의미를 해석하지 않는다.
3. `Message`에서 발신자의 차례·행동 가능 여부와 payload 길이·값을 검사한 뒤 서버 상태를 갱신한다.
4. `send`로 개인 응답을, `broadcast`로 룸 전체가 알아야 할 상태를 보낸다.
5. `Joined`와 `Reconnected`에서 현재 상태를 보내 클라이언트 화면을 초기화한다.

예제는 opcode 1과 빈 payload를 입력받아 값을 하나 늘린다. 서버가 보내는 opcode 1의 payload는 8바이트 big-endian u64 값이다. 같은 opcode라도 방향별 payload 규칙이 다르며, 이 규약은 예제 게임이 정한 것이다. 실제 게임은 자신의 명령과 상태 형식을 정의한다. `GuestAuth`와 `server::router`를 연결하는 예제의 `main`도 그대로 출발점으로 사용할 수 있다.

| 메서드 | 동작 |
|---|---|
| `admit(&self, player: &Player) -> bool` | 입장 직전 게임 정책, 기본 허용. 정원은 엔진이 검사 |
| `tick_interval(&self) -> Option<Duration>` | 기본 None. 최소 1ms, 밀린 tick은 건너뜀 |
| `on_event(&mut self, GameEvent, &mut RoomContext)` | 룸 task에서 순차 실행 |

| GameEvent | 의미 |
|---|---|
| `Joined(Player)` | 최초 입장. 이미 참가자 목록에 포함됨. 초기 상태 전송 |
| `Disconnected(SessionId)` | 일시 연결 종료. 참가·게임 상태 유지 |
| `Reconnected(Player)` | 같은 논리 세션의 연결 복구. 현재 상태 스냅샷 전송 |
| `Left(SessionId)` | 최종 퇴장·유예 만료. 이미 참가자 목록에서 제거됨 |
| `Closing` | 정상 룸 종료. 필요한 영속화 작업을 bounded worker 큐에 제출 |
| `Message { player, opcode, payload: Bytes }` | 인증된 참가자의 게임 입력 |
| `System { opcode, payload: Bytes }` | 신뢰된 비동기 게임 작업 결과 |
| `Tick` | 선택적 주기 이벤트 |

`Message.player`는 서버가 정한다. 콜백에서 I/O·DB 호출·sleep·긴 CPU 작업을 하지 않는다. 룸 상태는 재접속 유예 동안 유지되지만 오프라인 출력과 자동 스냅샷은 없다. `Joined`와 `Reconnected`에서 게임이 현재 상태를 보내야 한다. `Closing`은 패닉·프로세스 강제 종료 시 실행을 보장하지 않으므로 영속화 완료 ACK로 취급하지 않는다.

이벤트와 모든 방의 코드가 한 스레드에서만 실행된다는 뜻은 아니다. **한 방의 콜백끼리** 순차 실행되므로 그 방의 게임 상태를 변경할 때 별도 락을 둘 필요가 없다는 의미다. `Tick`은 선택한 간격의 진행 이벤트로, 항상 정확한 횟수나 시각을 보장하는 시계는 아니다.

## RoomContext 전송

| 메서드 | 범위·반환 |
|---|---|
| `send(session, opcode, payload) -> Result<()>` | 현재 룸 참가자 한 명. 비참가자는 `NotMember` |
| `send_many(sessions, opcode, payload) -> usize` | 지정 참가자. 중복 제거 후 출력 큐에 넣은 수 |
| `broadcast(opcode, payload) -> usize` | 현재 룸 전원 |
| `broadcast_except(session, opcode, payload) -> usize` | 지정 참가자 제외 |
| `send_json(session, opcode, &value)` | JSON을 GAME payload로 직렬화해 한 명에게 전송 |
| `broadcast_json(opcode, &value)` | JSON을 GAME payload로 직렬화해 룸 전원에게 전송 |
| `players()` | 연결이 끊긴 참가자도 포함하는 Player iterator |
| `host()` | 최초 생성자·첫 매칭 후보의 session ID. 최종 퇴장 후 None, 자동 승계 없음 |
| `room_id()` / `handle()` | 현재 룸 ID / 비동기 결과 전달용 RoomHandle clone |
| `close()` | 콜백 뒤 게임방 종료 요청. 글로벌에는 적용 안 됨 |

`RoomContext.send`와 broadcast 계열은 **게임 payload만** 받으며 현재 room ID와 opcode가 들어간 GAME 헤더를 엔진이 붙인다. JSON 편의 메서드도 tag가 붙은 제어 JSON이 아니라 GAME payload를 만든다.

바이너리 규약이 부담스럽다면 JSON payload로 시작할 수 있다. 서버는 `ctx.send_json(player, 100, &state)` 또는 `ctx.broadcast_json(100, &state)`를 호출하고, 브라우저는 `game` 이벤트의 `payload`를 `TextDecoder`와 `JSON.parse`로 해석한다. 여기서 `state`는 게임이 정의한 `serde::Serialize` 값이다. 클라이언트도 `sendJSON(roomId, opcode, value)`로 같은 방식의 payload를 보낼 수 있다. 입력 JSON의 역직렬화와 값 검사는 게임로직이 담당한다.

`ClientHandle.send(frame: Bytes)`는 **이미 인코딩한 전체 프레임**을 받는 신뢰된 서버 API다. 자동 GAME 헤더·룸 멤버십 검사가 없으므로 `protocol::GameFrame::encode()` 또는 `protocol::json()`으로 프레임을 만들어야 한다. 브라우저의 임의 수신자 지정 기능으로 노출하지 않는다.

send 성공은 mailbox 수용이며 네트워크 수신 확인(ACK)이 아니다. 오프라인 수신자는 `Closed`, 꽉 찬 큐는 `Busy`이며 해당 연결은 종료 신호를 받는다. 여러 수신자에게 보내는 동작은 다른 수신자를 계속 처리하고 한 번 인코딩한 `Bytes`를 공유한다. `Bytes::clone()`은 전체 데이터를 복사하는 대신 같은 바이트 저장소를 공유하는 핸들을 만든다. 중요한 결과의 확인은 게임별 ACK로 구현한다. Rust API의 `Error::Busy` 같은 오류는 통신 규약에서 `busy` 같은 문자열로 표현된다.

## 비동기 게임 작업

콜백은 미리 만든 bounded worker 큐에 `try_send`로 작업을 제출한다. 큐 포화 정책과 timeout·동시 실행 상한은 게임이 정한다. worker가 계산·I/O를 끝내면 다음 경로로 결과를 반환할 수 있다.

```rust
// 이미 제한된 worker 안에서 실행한다.
let result = bytes::Bytes::from_static(b"result");
let outcome = room.event(GameEvent::System { opcode: 100, payload: result });
// Busy / RoomNotFound일 때의 폐기·재시도 정책은 게임이 결정한다.
let _ = outcome;
```

`room`은 콜백에서 얻은 `ctx.handle()`이다. `RoomHandle::event`는 신뢰된 서버 코드 전용이며 클라이언트 패킷으로 직접 호출하게 노출하지 않는다. 콜백마다 제한 없이 task를 생성하지 않는다. 종료 저장이 필요하면 `Closing`에서 소유한 상태를 worker에 넘기고 worker 수명·종료 대기 정책을 애플리케이션이 관리한다.

worker는 방 밖에서 오래 걸리는 일을 수행하는 작업자다. 결과를 돌려보낼 때도 worker가 방 상태를 직접 변경하지 않고 `System` 메시지를 보내 룸 콜백에서 반영한다. 외부 작업 중 턴이나 라운드가 바뀔 수 있으므로 payload에 작업 ID·상태 버전을 넣고 이미 유효하지 않은 결과는 게임이 버린다. 엔진이 결과의 게임상 유효성을 판정하지는 않는다.

## 논리 세션과 Engine API

연결 종료는 세션의 최종 종료와 다르다. 서버는 Ping 20초·Pong 대기 10초로 연결을 확인하고, 끊긴 연결을 suspend한 뒤 별도 재접속 유예 60초를 적용한다. 유예 중에도 세션 상한과 방 정원을 차지한다. 대기열에서는 제거하며 오프라인 프레임은 저장하지 않는다.

| 메서드 | 의미 |
|---|---|
| `connect(principal, name)` / `connect_identity(identity)` | 새 논리 SessionId 발급·등록·글로벌 입장 |
| `resume(id, token: &str, identity: Option<Identity>)` | 세션 복구·이전 연결 교체·토큰 회전. 게스트는 None, 계정은 같은 계정의 새 인증 결과 필요 |
| `suspend(id, generation)` | 해당 연결 세대의 종료만 반영. 참가 유지·Disconnected 전달 |
| `disconnect(id)` | 신뢰된 서버의 최종 해제. 세션·대기열·모든 방 참가 제거 |
| `create(&client, capacity)` / `join(&client, room_id)` | 실제 입장 완료까지 기다림 |
| `leave(&client, room_id)` | 최종 퇴장. 이미 없으면 성공, 글로벌은 거절 |
| `queue(&client)` / `cancel_queue(id)` | FIFO 참가·대기 취소 |
| `stats()` | 논리 sessions·rooms·queued 집계. 원자적 스냅샷 아님 |
| `shutdown()` | 새 접속 차단·클라이언트 취소·관리 task 종료 대기 |

WebSocket 어댑터가 `suspend`를 호출한다. 직접 임베딩하는 전송 계층도 연결 종료 시 받은 `ConnectedClient.generation`으로 suspend해야 한다. 오래된 연결의 종료 정리는 새 연결을 끊지 않는다. `disconnect`는 일시 네트워크 손실이 아닌 강제 퇴장·최종 정리에 사용한다. `ClientHandle.close()`는 현재 연결 취소이며 전송 소유자의 suspend 처리가 뒤따라야 한다.

한 계정은 프로세스당 한 논리 세션만 허용한다. resume은 최신 토큰과 계정의 새 인증 결과를 확인하며 기존 Player 스냅샷을 유지한다. 재접속 AUTH와 브라우저 SDK 사용법은 [프로토콜](protocol.md)을 따른다. 상태 복구는 같은 프로세스 안에서만 가능하다.

## 인증·신뢰된 M:N 전달

M:N 전달은 여러 발신자와 여러 수신자 사이에서 메시지를 전달하는 기능이다. 룸 참가자에게 보내는 일반 게임 출력은 `RoomContext`를 사용하고, 룸 밖의 특정 세션 목록을 대상으로 해야 할 때만 아래 서버 API를 사용한다. 어떤 대상에게 보낼 수 있는지는 신뢰된 서버 코드가 결정한다.

`Authenticator::authenticate(AuthRequest)`는 `Identity { principal, name, avatar_url }`를 반환한다. 인증 방식과 외부 I/O는 애플리케이션 어댑터에 두고 룸 콜백과 게임 패킷 경로에 넣지 않는다. 추가 제어 요청은 `server::Application`과 `router_with_application`으로 확장할 수 있으며 GAME은 이 경로를 거치지 않는다.

기본 `GuestAuth`는 새 게스트 인증만 제공한다. 계정 티켓의 발급·검증 방식은 포함되어 있지 않다. 애플리케이션이 티켓을 검증해 `Principal::Account`와 프로필을 반환해야 한다. 티켓이나 인증 서비스 검증이 실패했다고 게스트로 바꿔서는 안 된다. `Player`의 프로필은 인증 시의 값이며 엔진이 외부 사이트에서 계속 갱신하지 않는다.

`Application`은 저장·조회 등 게임 진행과 별개인 추가 제어 요청이 필요할 때 붙이는 선택적 훅이다. 서버가 처리할 tag·요청/응답 형식과 접근 권한을 애플리케이션이 정한다. 기본 제어 요청과 이 훅의 요청은 연결당 동시에 하나만 처리하며, 처리 중 추가 요청에는 `busy`가 반환될 수 있다. 일반적인 게임 행동은 `GameLogic`과 GAME 패킷으로 구현한다.

`engine.sessions().resolve(ids).await?`는 논리 ClientHandle을 중복 없이 조회한다. 요청당 최대 4096개다. 인코딩한 전체 프레임을 여러 handle의 `send(frame.clone())`로 전달할 수 있다. handle은 재접속 후에도 같은 논리 세션을 가리키며 오프라인 상태에서는 전송이 실패한다. `online(account_ids)`는 이 프로세스의 활성 연결 여부를 입력 순서대로 반환한다.

빈 게임방은 회수한다. 게임방 패닉은 해당 방을 제거하고 참가자에게 RoomClosed를 알린다. 글로벌 룸 패닉은 엔진을 비가용으로 만든다. factory 패닉은 생성 요청 실패로 격리한다. `panic=abort` 빌드에는 이 패닉 격리가 적용되지 않는다.
