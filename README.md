# Rust 게임서버 엔진

Rust와 Tokio로 만든, 하나의 서버 프로세스에서 운영하는 멀티플레이 게임서버 엔진입니다. 플레이어 연결, 룸, 매칭 등은 엔진이 처리하고 게임 규칙은 `GameLogic`으로 구현합니다.

## 액터 모델

웹 API는 주로 요청과 응답으로 동작하지만, 실시간 웹게임 서버는 상대의 행동도 사용자에게 즉시 전달하기 위해 WebSocket 연결을 유지해야 합니다. 

여러 사용자의 게임 상태를 동시에 변경하려면 처리 순서를 정해야 합니다. 공유 상태에 락을 건 채 네트워크 I/O를 기다리면, 같은 상태가 필요한 다른 작업들도 기다리게 됩니다.

액터 모델은 상태를 각 액터가 소유하고 메시지로 작업을 요청하여 이런 공유와 대기를 줄입니다. 각 액터는 독립적으로 메모리를 가지고, 자신의 메시지 큐를 순서대로 처리합니다. 예를 들어 한 방의 이동과 공격은 같은 룸 액터가 차례로 처리하고, 서로 다른 방은 독립적으로 비동기 진행합니다.

이 구현에서 룸과 매니저는 Tokio의 비동기 작업(task)과 크기가 제한된 메시지 큐(mailbox)로 동작합니다.

이 엔진에 게임별 `GameLogic`을 생성하는 함수(factory)를 전달하면 세션·룸·매칭·WebSocket 처리를 재사용할 수 있습니다. [구조 안내](docs/architecture.md)에서 각 액터의 역할과 메시지 흐름을 설명합니다.

## 실행

로컬 실행에는 Rust와 Cargo가 필요합니다. Docker 빌드는 Rust 1.91.1을 사용합니다.

```bash
cargo run --release
# ws://localhost:8080/ws, /health/live, /health/ready

ALLOWED_ORIGINS=http://localhost:3000 cargo run --release
cargo run --release --example custom_game
```

기본 바이너리는 외부 서비스 없이 실행하는 게스트용 EchoGame이다. 계정 인증은 게임 애플리케이션이 `Authenticator`를 구현해 주입한다. [`examples/custom_game.rs`](examples/custom_game.rs)는 카운터 변경과 입장·재접속 스냅샷을 보여 준다. 이 예제의 Origin 허용 등 배포 설정은 `Config`에서 정한다.

Docker로 실행하려면 다음 명령을 사용한다.

```bash
docker compose up --build
```

기본 Compose 설정은 `http://localhost:3000`에서 열어 둔 웹페이지의 접속을 허용한다. Origin은 페이지 주소의 프로토콜·호스트·포트 조합으로, 경로는 포함하지 않는다. `ALLOWED_ORIGINS`는 이 출처를 쉼표로 구분한 목록이며 WebSocket 주소가 아니다. 다른 주소의 페이지에서는 이 값을 바꿔 실행한다. 브라우저 연결 예제는 [프로토콜·SDK 안내](docs/protocol.md#브라우저-sdk)를 따른다. HTTP 주소 `/health/live`는 프로세스 응답 여부, `/health/ready`는 엔진의 접속 가능 여부를 확인한다.

## 구조와 개발 문서

- [구조·용어·처리 흐름](docs/architecture.md): 세션과 연결의 차이, 글로벌 룸, 각 액터의 책임.
- [액터 인터페이스·게임 확장](docs/interfaces.md): GameLogic, 룸 전송, 비동기 작업, 논리 세션과 연결 수명.
- [WebSocket 프로토콜·브라우저 SDK](docs/protocol.md): 인증·재접속, 제어 요청, 바이너리 GAME 프레임.

서버의 접속 처리 task는 참가가 승인된 방의 `RoomHandle`로 게임 입력을 전달한다. `RoomHandle`은 룸에 메시지를 보내는 서버 내부 핸들이며 브라우저가 직접 접근하는 객체는 아니다. 룸은 단일 task가 상태를 소유하고 크기가 제한된 mailbox를 사용한다. 출력 큐가 가득 차면 해당 연결을 종료하며 다른 수신자 처리는 계속한다. 일시 연결 종료 시 논리 세션과 방 참가 상태는 재접속 유예 시간 동안 유지된다. 오프라인 출력은 저장하지 않으므로 게임은 `Reconnected`에서 현재 상태를 전송한다.

## 설정

기본 바이너리의 환경변수다. 라이브러리 사용자는 대응하는 `Config` 필드를 직접 설정한다.

| 환경변수 | 기본값 | 의미 |
|---|---|---|
| HOST / PORT | 0.0.0.0 / 8080 | 서버 listen |
| ALLOWED_ORIGINS | 비어 있음 | 정확한 브라우저 Origin 목록. 비어 있으면 Origin 없는 개발 도구만 허용 |
| ALLOW_GUESTS | true | 기본 예제는 게스트만 지원하며 false이면 시작 거절 |
| MAX_SESSIONS | 10000 | 연결 중·재접속 유예 중인 논리 세션 상한 |
| MAX_ROOMS | 2000 | 글로벌 포함 방 수 |
| MAX_ROOM_PLAYERS | 64 | 게임방 정원 상한 |
| MAX_ROOMS_PER_SESSION | 2 | 글로벌 포함 세션당 참가 방 수 |
| ROOM_MAILBOX_CAPACITY | 256 | 룸 mailbox 메시지 수 |
| OUTBOUND_CAPACITY | 128 | 연결별 출력 mailbox 이벤트 수 |
| MAX_FRAME_BYTES | 65536 | WebSocket 입력·게임 출력 크기 상한 |
| MATCH_SIZE | 2 | FIFO 매칭 인원, 0이면 비활성 |
| MESSAGES_PER_SECOND | 120 | 연결별 입력 상한. Ping/Pong 포함 |
| ALLOW_CREATE / ALLOW_JOIN / ALLOW_QUEUE | true | `Config.allow_create` / `allow_join` / `allow_queue`. false인 동작은 `denied` |
| ALLOW_GLOBAL_INPUT | false | `Config.allow_global_input`. room 0으로 들어오는 클라이언트 GAME 허용 여부 |
| PING_INTERVAL_SECONDS | 20 | 서버 WebSocket Ping 주기 |
| PONG_TIMEOUT_SECONDS | 10 | 보낸 Ping과 같은 payload의 Pong 대기 시간 |
| RECONNECT_GRACE_SECONDS | 60 | 연결 종료 후 논리 세션·룸 참가 상태 유지 시간 |
| TOKIO_WORKER_THREADS | 런타임 기본 | Tokio worker 수 |

`Config.auth_timeout`과 `write_timeout`은 각각 기본 5초다. 연결 생존 확인과 재접속 유예는 별도 설정이다. 브라우저는 WebSocket Pong을 자동 처리하므로 연결 유지를 위한 게임 입력이 필요 없다. 큐 수와 프레임 크기를 함께 고려해 메모리 예산을 정한다.

정원이 가득 차도 기존 세션을 복구할 수 있도록 인증 대기 슬롯 `min(MAX_SESSIONS, 128)`개를 별도로 둔다. 전체 소켓 상한은 논리 세션 상한과 이 슬롯 수의 합이다. `Config.idle_timeout`은 이전 코드의 컴파일 호환용이며 연결 생존 판단에는 Ping/Pong 설정을 사용한다.

## 검증

```bash
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
node --test tools/client.test.mjs

docker build --target test -t study-engine-test .
docker run --rm study-engine-test cargo test --release --locked --all-targets

# 활성 연결 중 Docker SIGTERM 종료 검증
docker build -t study-engine .
./tools/docker-smoke.sh study-engine
```

SDK 테스트는 Node.js 22 이상을 사용한다. WebSocket 통합 테스트는 로컬 TCP 포트를 열 수 있어야 한다. DB·Redis·별도 웹사이트 컨테이너는 필요 없다.

부하를 직접 측정할 때는 `./tools/benchmark.sh`를 사용할 수 있다. Docker가 필요하며 결과는 Git에서 제외되는 `target/benchmarks/local/`에 저장된다. `RESULT_DIR`로 저장 위치를 바꿀 수 있다. 이 측정은 예제 로직 기준이므로 실제 게임의 수용 인원은 자체 게임로직과 환경으로 확인한다.

## 확장 범위

영속 게임 상태, 분산 룸·서버 간 매칭, 방장 자동 승계, 관전자 정책, ELO와 패킷 압축은 게임이 정한다. 엔진의 재접속은 같은 프로세스의 논리 세션을 복원한다. 프로세스 재시작 복구, 오프라인 메시지 큐와 게임 상태 스냅샷 저장은 게임 애플리케이션이 구현한다. 룸 패닉은 해당 게임방을 정리하고, 글로벌 룸 패닉은 엔진을 비가용으로 바꾼다.
