# SPEC-REMOTE-001: Remote Notification Forwarding via SSH Tunnel + HTTP Server

---

## Metadata

| Field       | Value                                                       |
| ----------- | ----------------------------------------------------------- |
| SPEC ID     | SPEC-REMOTE-001                                             |
| Title       | Remote Notification Forwarding via SSH Tunnel + HTTP Server |
| Status      | Completed                                                   |
| Priority    | High                                                        |
| Created     | 2026-02-20                                                  |
| Author      | manager-spec                                                |
| Domain      | REMOTE                                                      |

---

## 1. 개요 (Overview)

### 1.1 문제 정의

원격 Linux 서버에서 Claude Code를 실행하는 사용자가 로컬 Windows PC에서 Agent Toast 데스크톱 알림을 수신하지 못한다. 현재 아키텍처는 Named Pipe IPC를 통해 동일 머신 내부에서만 동작하며, 네트워크를 통한 알림 전달 메커니즘이 존재하지 않는다.

### 1.2 해결 방안

Agent Toast 로컬 인스턴스에 경량 HTTP 서버를 내장하고, SSH 역방향 터널을 통해 원격 서버에서 전송된 알림을 수신하는 파이프라인을 구성한다.

```
[Remote Linux Server]                    [Local Windows PC]
Claude Code Hooks                        Agent Toast
    │                                         │
    ▼                                         │
curl POST /notify ──── SSH Tunnel ──────► HTTP Server (127.0.0.1:9876)
                    (Reverse Tunnel)          │
                                              ▼
                                       show_notification()
                                              │
                                              ▼
                                       Toast Notification
```

### 1.3 아키텍처 결정: SSH Tunnel + HTTP

**채택 이유:**
- 별도 서버 인프라 불필요 (로컬 전용 바인딩으로 보안 유지)
- 기존 `show_notification()` 파이프라인 완전 재사용
- `source: "remote"` 추가로 Win32 프로세스 트리 탐색 우회 가능
- 사용자가 이미 보유한 SSH 인증 인프라 활용
- `std::process::Command`로 ssh 프로세스 스폰 (대형 SSH 라이브러리 의존성 불필요)

**대안 검토:**
- WebSocket: 방화벽 복잡성 증가, 별도 포트 노출 필요
- MQTT/메시지 큐: 추가 서버 인프라 필요
- 직접 TCP: SSH 암호화 및 인증 부재

---

## 2. 환경 (Environment)

### 2.1 로컬 환경 (Windows PC)
- Agent Toast v0.1.3 이상 실행 중
- Windows 10 Build 1909 / Windows 11
- OpenSSH 클라이언트 설치됨 (Windows 10 1809+에 기본 내장)
- 방화벽: 127.0.0.1:9876 (localhost only, 외부 노출 없음)

### 2.2 원격 환경 (Linux Server)
- SSH 서버 실행 중 (sshd with GatewayPorts 설정)
- curl 설치됨 (알림 전송용)
- Claude Code 실행 환경 (hook 지원)
- 포트 19876 사용 가능 (터널 원격 측 포트)

### 2.3 네트워크
- SSH 연결: 로컬 → 원격 (기본 포트 22)
- 역방향 터널: 원격:19876 → 로컬:9876
- HTTP 서버: 127.0.0.1:9876 (localhost only)

---

## 3. 가정 (Assumptions)

| ID    | 가정                                                                              | 신뢰도 | 위험 시나리오                                  |
| ----- | --------------------------------------------------------------------------------- | ------ | ---------------------------------------------- |
| A-001 | Windows에 OpenSSH 클라이언트(`ssh.exe`)가 PATH에 있음                             | 높음   | 구버전 Windows는 별도 설치 필요                |
| A-002 | 원격 서버 sshd의 `GatewayPorts` 설정이 `yes` 또는 `clientspecified`              | 중간   | 기본값 `no`면 원격 포트 바인딩 실패            |
| A-003 | SSH 키 기반 인증 사용 (비밀번호 불지원)                                           | 높음   | 자동 재연결 시 비밀번호 입력 불가              |
| A-004 | 원격 서버에 `curl`이 설치되어 있음                                                | 높음   | 일부 최소 설치 환경에는 없을 수 있음           |
| A-005 | `tiny_http` crate 또는 표준 라이브러리 `TcpListener`로 HTTP 서버 구현 가능        | 높음   | tokio/axum 추가 시 의존성 증가                |
| A-006 | 기존 Named Pipe 흐름은 변경 없이 유지됨                                           | 높음   | 구조적 변경으로 기존 기능 영향 없어야 함       |
| A-007 | 토큰은 32자 랜덤 hex 문자열로 충분한 보안 수준 제공                               | 높음   | 브루트포스는 SSH 터널이 방어                   |

---

## 4. 요구사항 (Requirements)

### R1: HTTP 알림 수신 서버

**R1.1 (Ubiquitous):** 시스템은 `remote_enabled = true`일 때 항상 `127.0.0.1:{remote_port}`에서 HTTP 서버를 실행해야 한다.

**R1.2 (Event-Driven):** WHEN `POST /notify` 요청이 유효한 `X-Agent-Toast-Token` 헤더와 함께 수신되면 THEN 시스템은 요청 본문을 `NotifyRequest`로 역직렬화하여 `show_notification()`을 호출해야 한다.

**R1.3 (Unwanted):** 시스템은 `X-Agent-Toast-Token` 헤더가 없거나 일치하지 않는 요청에 대해 `401 Unauthorized`를 반환하고 알림을 표시하지 않아야 한다.

**R1.4 (Unwanted):** 시스템은 `127.0.0.1` 이외의 주소로 HTTP 서버를 바인딩하지 않아야 한다 (보안 격리).

**R1.5 (Event-Driven):** WHEN HTTP 서버가 역직렬화 실패 요청을 수신하면 THEN 시스템은 `400 Bad Request`를 반환하고 오류를 로그에 기록해야 한다.

**R1.6 (Ubiquitous):** 시스템은 `source: "remote"` 값을 가진 `NotifyRequest`에 대해 Win32 프로세스 트리 탐색을 건너뛰어야 한다 (`is_internal` 패턴과 동일).

### R2: SSH 역방향 터널 관리

**R2.1 (State-Driven):** IF `ssh_auto_connect = true` AND Agent Toast 시작 시 THEN 시스템은 자동으로 SSH 역방향 터널을 수립해야 한다.

**R2.2 (Event-Driven):** WHEN SSH 터널 프로세스가 종료되면 THEN 시스템은 설정된 재시도 간격 후 자동으로 재연결을 시도해야 한다.

**R2.3 (Ubiquitous):** 시스템은 터널 연결 상태 (연결됨/연결 끊김/오류)를 내부적으로 추적해야 한다.

**R2.4 (Unwanted):** 시스템은 SSH 비밀번호 인증을 지원하지 않아야 한다 (키 기반 인증 전용).

**R2.5 (Event-Driven):** WHEN 사용자가 Settings UI에서 "Connect" 버튼을 클릭하면 THEN 시스템은 즉시 SSH 터널 수립을 시도해야 한다.

**R2.6 (Event-Driven):** WHEN 사용자가 Settings UI에서 "Disconnect" 버튼을 클릭하면 THEN 시스템은 SSH 터널 프로세스를 종료해야 한다.

**R2.7 (Ubiquitous):** SSH 터널 명령은 다음과 동등한 형식이어야 한다:
`ssh -R {ssh_remote_port}:127.0.0.1:{remote_port} {ssh_user}@{ssh_host} -p {ssh_port} -N -i {ssh_key_path} -o StrictHostKeyChecking=accept-new -o ServerAliveInterval=30`

### R3: 설정 관리

**R3.1 (Ubiquitous):** 시스템은 `HookConfig`에 다음 원격 설정 필드를 포함해야 한다:

| 필드명             | 타입   | 기본값  | 설명                         |
| ------------------ | ------ | ------- | ---------------------------- |
| `remote_enabled`   | bool   | false   | 원격 알림 기능 활성화        |
| `remote_port`      | u16    | 9876    | 로컬 HTTP 서버 포트          |
| `remote_token`     | String | ""      | Bearer 인증 토큰             |
| `ssh_host`         | String | ""      | SSH 서버 호스트명/IP         |
| `ssh_port`         | u16    | 22      | SSH 서버 포트                |
| `ssh_user`         | String | ""      | SSH 사용자명                 |
| `ssh_key_path`     | String | ""      | SSH 개인키 경로              |
| `ssh_remote_port`  | u16    | 19876   | 원격 터널 포트               |
| `ssh_auto_connect` | bool   | false   | 앱 시작 시 자동 연결         |

**R3.2 (Event-Driven):** WHEN `remote_enabled`가 처음 true로 설정되면 THEN 시스템은 32자 랜덤 hex 토큰을 자동 생성하여 `remote_token`에 저장해야 한다.

**R3.3 (Ubiquitous):** 시스템은 `#[serde(default)]` 어트리뷰트를 사용하여 모든 원격 설정 필드에 하위 호환성을 보장해야 한다.

**R3.4 (Unwanted):** 시스템은 `remote_token` 값을 로그 파일에 평문으로 기록하지 않아야 한다.

### R4: Linux 측 설정 가이드

**R4.1 (Where possible):** 가능하면 시스템은 Settings UI에서 원격 서버용 curl 명령 템플릿을 표시해야 한다:

```bash
curl -s -X POST \
  -H "X-Agent-Toast-Token: {remote_token}" \
  -H "Content-Type: application/json" \
  -d '{"pid":0,"event":"$EVENT","message":"$MESSAGE","source":"remote"}' \
  http://localhost:{ssh_remote_port}/notify
```

**R4.2 (Where possible):** 가능하면 시스템은 Claude Code `~/.claude/settings.json` hook 설정 스니펫 예시를 제공해야 한다.

### R5: 연결 상태 및 진단

**R5.1 (Ubiquitous):** 시스템은 설정 UI에서 현재 SSH 터널 연결 상태를 표시해야 한다.

**R5.2 (Event-Driven):** WHEN 사용자가 "Test Connection" 버튼을 클릭하면 THEN 시스템은 테스트 알림 요청을 자체 HTTP 서버로 전송하고 결과를 UI에 표시해야 한다.

**R5.3 (Event-Driven):** WHEN 원격 알림이 수신되면 THEN 시스템은 타임스탬프와 이벤트 유형을 포함한 수신 로그를 기록해야 한다 (토큰 제외).

---

## 5. 비기능 요구사항 (Non-Functional Requirements)

### 성능
- HTTP 서버 요청 처리: 100ms 이내
- SSH 터널 재연결 기본 간격: 10초
- HTTP 서버는 별도 스레드에서 실행 (메인 Tauri 루프 블로킹 없음)

### 보안
- HTTP 서버는 127.0.0.1 바인딩 전용 (외부 노출 없음)
- SSH 터널을 통해서만 원격 접근 가능
- 토큰 기반 인증으로 무단 접근 차단
- 토큰은 로그에 마스킹 처리

### 호환성
- 기존 Named Pipe 흐름 완전 유지
- 기존 알림 파이프라인 (`show_notification()`) 재사용
- `HookConfig` 하위 호환성 (기존 설정 파일 파싱 가능)

### 의존성
- **권장:** `tiny_http` crate (비동기 런타임 불필요, 경량)
- **대안:** 표준 라이브러리 `TcpListener` + 수동 HTTP 파싱
- **금지:** `tokio`, `axum`, `warp` (대형 비동기 런타임 추가)
- `std::process::Command`로 `ssh` 프로세스 스폰 (libssh2 등 불필요)

---

## 6. 트레이서빌리티 (Traceability)

| 요구사항 ID | 구현 파일                         | 테스트 참조          |
| ----------- | --------------------------------- | -------------------- |
| R1.1-R1.6   | `src-tauri/src/remote.rs`         | `acceptance.md#S1`   |
| R2.1-R2.7   | `src-tauri/src/remote.rs`         | `acceptance.md#S2`   |
| R3.1-R3.4   | `src-tauri/src/setup.rs`          | `acceptance.md#S3`   |
| R4.1-R4.2   | `src/components/RemoteSettings.vue` | `acceptance.md#S4` |
| R5.1-R5.3   | `src-tauri/src/remote.rs`, `src/components/RemoteSettings.vue` | `acceptance.md#S5` |

---

## 7. 변경 파일 목록 (Proposed File Changes)

### 신규 파일
- `src-tauri/src/remote.rs` - HTTP 서버 + SSH 터널 관리 모듈
- `src/components/RemoteSettings.vue` - 원격 설정 UI 탭

### 수정 파일
- `src-tauri/src/setup.rs` - `HookConfig`에 원격 설정 필드 추가
- `src-tauri/src/lib.rs` - `remote` 모듈 등록, IPC 커맨드 추가
- `src-tauri/src/notification.rs` - `source: "remote"` 처리
- `src-tauri/Cargo.toml` - `tiny_http` 또는 관련 의존성 추가
- `src/Setup.vue` - Remote 탭 추가
- `src/types.ts` - `HookConfig` 인터페이스에 원격 필드 추가
- `src/i18n.ts` (또는 locale 파일) - 원격 설정 i18n 문자열 추가

---

## 8. Implementation Notes (v0.1.7)

### 추가 구현 사항 (SPEC 범위 외)

| 기능 | 설명 | 관련 파일 |
| ---- | ---- | --------- |
| `remote_host` 표시 | 원격 알림에 SSH 서버 호스트명 표시 | `cli.rs`, `notification.rs`, `App.vue` |
| `save_remote_config` | 원격 설정만 독립 저장 (hooks 재생성 없음) | `setup.rs`, `RemoteSettings.vue` |
| SSH `-R *:port` 바인딩 | 모든 인터페이스에 바인딩하여 다른 서버에서도 접근 가능 | `remote.rs` |
| sshd_config 가이드 | GatewayPorts 설정 안내 UI 추가 | `RemoteSettings.vue`, locale 파일 |
| 알림 스타일 커스터마이징 | 배경 불투명도, 배경색, 텍스트색 설정 | `setup.rs`, `GeneralSettings.vue`, `App.vue` |

### SSH 터널 바인딩 변경

원래 SPEC의 R2.7에서 정의한 `-R {ssh_remote_port}:127.0.0.1:{remote_port}` 형식이 `-R *:{ssh_remote_port}:127.0.0.1:{remote_port}`로 변경되었다. 이는 원격 서버의 모든 인터페이스에 터널 포트를 바인딩하여, 같은 네트워크의 다른 서버에서도 알림을 전송할 수 있게 한다. `GatewayPorts yes` sshd 설정이 전제 조건이다.

---

*SPEC-REMOTE-001 | manager-spec | 2026-02-20*
