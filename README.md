# CursorDrop (Rust)

AutoHotkey v2 스크립트(`CursorDrop v4`)를 **Rust + windows-sys**로 포팅한
단독 실행 Windows 유틸리티. 파일을 알약 위젯에 드래그하거나 `Ctrl+V`로
클립보드 이미지를 붙여 넣으면 → 원격 호스트로 SCP 업로드하고 → 그 **원격
절대경로를 클립보드에 복사**한다. 터미널(WezTerm 등)에서 SSH로 접속해 돌리는
**원격 Claude Code**에 `Ctrl+Shift+V` 로 경로를 붙여 넣어 쓰는 용도.

## 동작 모드 (터미널 모드)

원본 AHK는 Cursor/VS Code GUI 창의 `[SSH: alias]` 타이틀을 읽어 원격 경로를
알아냈다. 이 포팅본은 **WezTerm + 원격 Claude Code** 환경에 맞춰 재설계됨:

- 에디터 창을 안 본다. 대신 exe 옆 **`CursorDrop.ini`** 에서 SSH alias / 원격
  디렉터리를 읽는다.
- 업로드 후 원격 절대경로를 **클립보드에만** 넣는다(자동 붙여넣기 없음 —
  포커스/탭 꼬임 방지). 사용자가 터미널에서 `Ctrl+Shift+V`.

## 핵심 기능

- 항상 위·반투명·둥근 모서리 알약 위젯 (드래그로 이동, 다크/라이트 자동)
- **드래그 드롭** → 업로드
- **Ctrl+V** (위젯 포커스 상태) → 클립보드 파일 / 비트맵 이미지(PNG 변환) 업로드
- 원격 `$HOME` 1회 조회로 `~` → 절대경로 변환 (캐시)
- 백그라운드 스레드에서 `mkdir -p` + `touch` + `scp` (모두 `BatchMode=yes`)
- 트레이 아이콘 메뉴 (Paste / Show log / Exit)
- 상태 색상 피드백 (idle / reading / uploading / success / error)
- **CLI 모드**: `CursorDrop.exe <파일> [...]` → GUI 없이 업로드 후 종료

## 빌드

```powershell
cargo build --release
```

산출물: `target\release\CursorDrop.exe` (약 **340 KB**, 단독 실행).
MSVC 정적 CRT 링크(`.cargo/config.toml`) → `vcredist` 불필요. 참조 DLL은 전부
Windows 표준(kernel32 / user32 / gdi32 / gdiplus / shell32 / advapi32).

## 설정 — `CursorDrop.ini`

첫 실행 시 exe 옆에 자동 생성된다.

```ini
[Remote]
Alias=myserver
RemoteDir=~/.cursor-drop-files
```

- `Alias` — `~/.ssh/config` 의 `Host` 별칭. **원격 Claude Code가 도는 호스트**.
- `RemoteDir` — 업로드 위치. `~` 는 원격 `$HOME` 으로 펼쳐짐. `/` 로 시작하면
  절대경로 그대로 사용. 홈 기준이면 어떤 프로젝트에서 Claude를 돌리든 절대경로로
  읽을 수 있어 무난하다.

## 사용

1. `CursorDrop.exe` 더블클릭 → 화면 중앙 알약 + 트레이 아이콘.
2. 파일을 위젯에 **드래그**하거나, 이미지 복사 후 위젯 클릭→**Ctrl+V**
   (또는 위젯 우클릭 → "Paste clipboard").
3. 업로드되고 원격 절대경로가 **클립보드**에 들어간다.
4. WezTerm(원격 Claude Code)에서 **`Ctrl+Shift+V`** 로 붙여 넣으면 끝.

- 위젯 클릭 드래그로 이동, `Esc` 종료, 트레이/우클릭 메뉴.
- 로그: exe 옆 `CursorDrop.log`.

### 사전 요구

- Windows 10/11의 OpenSSH `ssh` / `scp` (기본 포함).
- **키 기반 무암호 인증** 필수. 앱은 콘솔 없이 `BatchMode=yes` 로 실행하므로
  passphrase 프롬프트가 뜨면 멈춘다 → passphrase 없는 키이거나 `ssh-agent`에
  미리 로드돼 있어야 한다.
- `ssh <alias> "echo ok"` 가 암호 없이 통과하면 앱도 동작.

## 코드 구조

| 파일 | 역할 |
|------|------|
| `src/util.rs` | 순수 문자열 로직(셸 인용·파일명 정리) + 단위 테스트 |
| `src/sys.rs` | UTF-16 변환·타임스탬프·로그·경로 |
| `src/config.rs` | `CursorDrop.ini` 로드/기본생성 |
| `src/clipboard.rs` | 클립보드 파일/비트맵(GDI+ PNG) + 텍스트 설정 |
| `src/upload.rs` | 원격 `$HOME` 해석 + 경로계산 + 클립보드 + scp(워커 스레드) |
| `src/main.rs` | 윈도/WndProc/트레이/상태머신/입력/CLI |

테스트: `cargo test`.

## 검증 (myserver 실측)

CLI 모드로 실제 원격 왕복 확인 완료: 기본 ini 생성 → `$HOME` 해석
(`/home/ubuntu`) → 클립보드에 절대경로 → `mkdir`+`touch`+`scp` → 원격 파일
도착(내용 포함) → 정리. scp는 최신 OpenSSH의 SFTP 프로토콜이라 원격 경로를
**따옴표 없이** 전달한다(sanitize로 공백 제거됨).
