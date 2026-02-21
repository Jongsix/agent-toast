# Remote Notification Setup Guide / 원격 알림 설정 가이드

<p>
  <strong>한국어</strong> | <a href="#english">English</a>
</p>

---

## 개요

원격 Linux 서버에서 Claude Code를 실행하면서, 로컬 Windows PC에서 데스크톱 알림을 받을 수 있습니다.

```
[원격 Linux 서버]                         [로컬 Windows PC]
Claude Code Hooks                         Agent Toast
    │                                         │
    ▼                                         │
curl POST /notify ──── SSH 터널 ──────► HTTP 서버 (127.0.0.1:9876)
                    (역방향 터널)              │
                                              ▼
                                       데스크톱 알림 표시
```

## 사전 요구사항

| 항목 | 조건 |
|------|------|
| 로컬 PC | Windows 10/11, Agent Toast v0.1.3+ 설치 |
| 원격 서버 | SSH 서버 실행 중, `curl` 설치됨 |
| 인증 | SSH 키 기반 인증 (비밀번호 미지원) |
| 네트워크 | 로컬 → 원격 SSH 연결 가능 |

## 설정 단계

### 1단계: Agent Toast 원격 기능 활성화

1. Agent Toast 설정 창을 엽니다 (트레이 아이콘 우클릭 → 설정, 또는 `agent-toast.exe --setup`)
2. **Remote** 탭으로 이동합니다
3. **원격 알림 활성화** 스위치를 켭니다
4. 자동으로 인증 토큰이 생성됩니다

### 2단계: SSH 연결 정보 입력

Remote 탭에서 다음 정보를 입력합니다:

| 설정 | 설명 | 예시 |
|------|------|------|
| SSH Host | 원격 서버 주소 | `dev-server.example.com` |
| SSH Port | SSH 포트 | `21168` (기본값) |
| SSH User | SSH 사용자명 | `aicc` (기본값) |
| SSH Key Path | SSH 개인키 경로 | `C:\Users\사용자\.ssh\id_rsa` |
| Local Port | 로컬 HTTP 서버 포트 | `19876` (기본값) |
| Remote Port | 원격 터널 포트 | `19876` (기본값) |

### 3단계: SSH 터널 연결

1. **Connect** 버튼을 클릭합니다
2. 상태가 **Connected** (녹색)으로 변경되면 성공입니다
3. (선택) **Auto Connect** 를 활성화하면 Agent Toast 시작 시 자동으로 터널이 연결됩니다

### 4단계: 원격 서버에서 Claude Code Hook 설정

원격 서버에 SSH로 접속한 후, `~/.claude/settings.json` 파일에 hook을 추가합니다.

Agent Toast 설정 UI의 **Setup Guide** 섹션에서 복사 버튼을 눌러 아래와 동일한 설정을 복사할 수 있습니다.

```json
{
  "hooks": {
    "Stop": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "curl -s -X POST -H 'X-Agent-Toast-Token: YOUR_TOKEN' -H 'Content-Type: application/json' -d '{\"pid\":0,\"event\":\"task_complete\",\"message\":\"Task done\",\"source\":\"remote\"}' http://localhost:19876/notify"
          }
        ]
      }
    ]
  }
}
```

> `YOUR_TOKEN`은 Agent Toast 설정에서 생성된 토큰으로 자동 대체됩니다. Settings UI의 복사 버튼을 사용하세요.

#### 여러 이벤트에 Hook 추가

Claude Code는 다양한 hook 이벤트를 지원합니다. 원하는 이벤트마다 curl 명령을 추가할 수 있습니다:

```json
{
  "hooks": {
    "Stop": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "curl -s -X POST -H 'X-Agent-Toast-Token: YOUR_TOKEN' -H 'Content-Type: application/json' -d '{\"pid\":0,\"event\":\"task_complete\",\"message\":\"Task completed\",\"source\":\"remote\"}' http://localhost:19876/notify"
          }
        ]
      }
    ],
    "UserInputRequired": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "curl -s -X POST -H 'X-Agent-Toast-Token: YOUR_TOKEN' -H 'Content-Type: application/json' -d '{\"pid\":0,\"event\":\"user_input_required\",\"message\":\"Input needed\",\"source\":\"remote\"}' http://localhost:19876/notify"
          }
        ]
      }
    ],
    "Error": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "curl -s -X POST -H 'X-Agent-Toast-Token: YOUR_TOKEN' -H 'Content-Type: application/json' -d '{\"pid\":0,\"event\":\"error\",\"message\":\"Error occurred\",\"source\":\"remote\"}' http://localhost:19876/notify"
          }
        ]
      }
    ]
  }
}
```

### 5단계: 테스트

원격 서버에서 직접 curl로 테스트합니다:

```bash
curl -s -X POST \
  -H "X-Agent-Toast-Token: YOUR_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"pid":0,"event":"task_complete","message":"Test notification","source":"remote"}' \
  http://localhost:19876/notify
```

로컬 PC에 알림이 표시되면 설정 완료입니다.

Agent Toast 설정 UI의 **Test Connection** 버튼으로도 연결 상태를 확인할 수 있습니다.

## 원격 서버 sshd 설정 (GatewayPorts)

역방향 터널이 작동하려면 원격 서버의 sshd에서 `GatewayPorts`를 허용해야 할 수 있습니다.

```bash
# /etc/ssh/sshd_config 확인
sudo grep GatewayPorts /etc/ssh/sshd_config
```

localhost 바인딩만 사용하는 경우 (기본 설정), `GatewayPorts no` (기본값)로도 동작합니다. 원격 서버에서 `localhost:19876`으로만 접근하기 때문입니다.

만약 동작하지 않으면:

```bash
# /etc/ssh/sshd_config에 추가
GatewayPorts clientspecified

# sshd 재시작
sudo systemctl restart sshd
```

## 보안 구조

| 계층 | 보호 방식 |
|------|-----------|
| 네트워크 | HTTP 서버가 `127.0.0.1`에만 바인딩 (외부 접근 불가) |
| 전송 | SSH 터널을 통한 암호화 전송 |
| 인증 | `X-Agent-Toast-Token` 헤더로 토큰 검증 |

- 토큰은 32자 랜덤 hex 문자열입니다
- HTTP 서버는 localhost 전용이므로 SSH 터널 없이는 접근할 수 없습니다
- SSH 키 기반 인증만 지원합니다 (비밀번호 인증 미지원)

## 트러블슈팅

### 터널 상태가 "Disconnected"로 유지됨

1. SSH 키 경로가 올바른지 확인합니다
2. SSH 키에 패스프레이즈가 없는지 확인합니다 (자동 연결 시 입력 불가)
3. 수동으로 SSH 연결을 테스트합니다:
   ```bash
   ssh -i C:\Users\사용자\.ssh\id_rsa user@server -p 22 -N -R 19876:127.0.0.1:9876
   ```

### curl 테스트에서 "Connection refused"

1. SSH 터널이 Connected 상태인지 확인합니다
2. 원격 서버에서 포트가 열려 있는지 확인합니다:
   ```bash
   ss -tlnp | grep 19876
   ```
3. 포트 충돌이 없는지 확인합니다 (다른 서비스가 19876을 사용 중일 수 있음)

### 알림이 표시되지 않음

1. 토큰이 일치하는지 확인합니다 (Settings UI에서 복사한 값과 동일한지)
2. JSON 형식이 올바른지 확인합니다 (`source` 필드가 `"remote"`인지)
3. Agent Toast가 실행 중인지 확인합니다 (시스템 트레이에 아이콘)

### SSH 터널이 자주 끊김

Agent Toast는 10초 간격으로 터널 상태를 확인하고 자동 재연결합니다. 하지만 네트워크가 불안정하면:

1. SSH `ServerAliveInterval`이 30초로 설정되어 있습니다 (기본 내장)
2. 원격 서버의 `ClientAliveInterval` 설정을 확인합니다:
   ```bash
   # /etc/ssh/sshd_config
   ClientAliveInterval 60
   ClientAliveCountMax 3
   ```

---

<a id="english"></a>

## English

### Overview

Receive desktop notifications on your local Windows PC from Claude Code running on a remote Linux server.

```
[Remote Linux Server]                    [Local Windows PC]
Claude Code Hooks                        Agent Toast
    │                                         │
    ▼                                         │
curl POST /notify ──── SSH Tunnel ──────► HTTP Server (127.0.0.1:19876)
                    (Reverse Tunnel)          │
                                              ▼
                                       Desktop Notification
```

### Prerequisites

| Item | Requirement |
|------|-------------|
| Local PC | Windows 10/11, Agent Toast v0.1.3+ installed |
| Remote Server | SSH server running, `curl` installed |
| Auth | SSH key-based authentication (passwords not supported) |
| Network | Local → Remote SSH connection available |

### Setup Steps

#### Step 1: Enable Remote in Agent Toast

1. Open Settings (tray icon right-click → Settings, or `agent-toast.exe --setup`)
2. Go to the **Remote** tab
3. Enable the **Remote Notifications** switch
4. An auth token is auto-generated

#### Step 2: Enter SSH Connection Info

| Setting | Description | Example |
|---------|-------------|---------|
| SSH Host | Remote server address | `dev-server.example.com` |
| SSH Port | SSH port | `21168` (default) |
| SSH User | SSH username | `aicc` (default) |
| SSH Key Path | SSH private key path | `C:\Users\user\.ssh\id_rsa` |
| Local Port | Local HTTP server port | `19876` (default) |
| Remote Port | Remote tunnel port | `19876` (default) |

#### Step 3: Connect SSH Tunnel

1. Click **Connect**
2. Status changes to **Connected** (green) = success
3. (Optional) Enable **Auto Connect** for automatic tunnel on startup

#### Step 4: Configure Claude Code Hooks on Remote Server

SSH into the remote server and add hooks to `~/.claude/settings.json`:

```json
{
  "hooks": {
    "Stop": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "curl -s -X POST -H 'X-Agent-Toast-Token: YOUR_TOKEN' -H 'Content-Type: application/json' -d '{\"pid\":0,\"event\":\"task_complete\",\"message\":\"Task done\",\"source\":\"remote\"}' http://localhost:19876/notify"
          }
        ]
      }
    ]
  }
}
```

> Use the **Copy** button in Agent Toast Settings UI to get the command with your actual token.

#### Step 5: Test

From the remote server:

```bash
curl -s -X POST \
  -H "X-Agent-Toast-Token: YOUR_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"pid":0,"event":"task_complete","message":"Test notification","source":"remote"}' \
  http://localhost:19876/notify
```

If a notification appears on your Windows PC, setup is complete.

### Security

| Layer | Protection |
|-------|-----------|
| Network | HTTP server binds to `127.0.0.1` only (no external access) |
| Transport | Encrypted via SSH tunnel |
| Auth | `X-Agent-Toast-Token` header verification |

### Troubleshooting

- **Tunnel stays "Disconnected"**: Verify SSH key path, ensure no passphrase on key, test manual SSH connection
- **"Connection refused" on curl test**: Check tunnel is Connected, verify port 19876 is listening (`ss -tlnp | grep 19876`)
- **No notification shown**: Verify token matches, check JSON format (`"source": "remote"`), ensure Agent Toast is running
- **Frequent disconnects**: Agent Toast auto-reconnects every 10s. Check server's `ClientAliveInterval` setting
