# CursorDrop (Rust)

AutoHotkey v2 스크립트(`CursorDrop v4`)를 **Rust + windows-sys**로 포팅한
단독 실행 Windows 유틸리티. 파일을 알약 위젯에 드래그하거나 `Ctrl+V`로
클립보드 이미지를 붙여 넣으면 → 원격 호스트로 SCP 업로드하고 → 그 **원격
절대경로를 클립보드에 복사**한다. 터미널(WezTerm 등)에서 SSH로 접속해 돌리는
**원격 Claude Code**에 `Ctrl+Shift+V` 로 경로를 붙여 넣어 쓰는 용도.

> **Android/Termux 포팅**도 있다 — 드래그 드롭 대신 **공유 시트**로 같은 흐름을
> 구현한 별도 셸 스크립트. `android/` 폴더와 아래 [Android / Termux 포팅](#android--termux-포팅)
> 절을 참고. Rust 크레이트와 코드를 공유하지 않지만 `CursorDrop.ini` 형식·동작은 동일.

## 동작 모드 (터미널 모드)

원본 AHK는 Cursor/VS Code GUI 창의 `[SSH: alias]` 타이틀을 읽어 원격 경로를
알아냈다. 이 포팅본은 **WezTerm + 원격 Claude Code** 환경에 맞춰 재설계됨:

- 에디터 창을 안 본다. 대신 exe 옆 **`CursorDrop.ini`** 에서 SSH alias / 원격
  디렉터리를 읽는다.
- 업로드 후 원격 절대경로를 **클립보드에만** 넣는다(자동 붙여넣기 없음 —
  포커스/탭 꼬임 방지). 사용자가 터미널에서 `Ctrl+Shift+V`.

## 핵심 기능

- 항상 위·둥근 모서리 위젯 (드래그로 이동, 다크/라이트 자동, **per-monitor DPI** 대응)
- **드래그 드롭** → 업로드
- **Ctrl+V** (위젯 포커스 상태) → 클립보드 파일 / 비트맵 이미지(PNG 변환) 업로드
- 원격 `$HOME` 1회 조회로 `~` → 절대경로 변환 (캐시)
- 백그라운드 스레드에서 `mkdir -p` + `touch` + `scp` (모두 `BatchMode=yes`)
- 우클릭 메뉴 (서버 선택 / Paste / Flush remote files / Show log / Exit)
- **다중 서버**: ini에 여러 서버를 정의하고 우클릭 메뉴에서 활성 서버 전환
- 상태 표시: 좌측 액센트 레일 색 + 전송 중 하단 진행 막대 (idle / uploading / success / error)
- **CLI 모드**: `CursorDrop.exe <파일> [...]` → GUI 없이 업로드 후 종료

## 빌드

```powershell
cargo build --release
```

산출물: `target\release\CursorDrop.exe` (단독 실행).
MSVC 정적 CRT 링크(`.cargo/config.toml`) → `vcredist` 불필요. 참조 DLL은 전부
Windows 표준(kernel32 / user32 / gdi32 / gdiplus / shell32 / advapi32).

## 설정 — `CursorDrop.ini`

첫 실행 시 exe 옆에 자동 생성된다. 서버마다 `[Server:<이름>]` 섹션을 두며,
섹션 이름이 우클릭 메뉴의 라벨로 쓰인다.

```ini
[Server:prod]
Alias=myserver
RemoteDir=~/.cursor-drop-files

[Server:dev]
Alias=devbox
RemoteDir=~/uploads
```

- `Alias` — `~/.ssh/config` 의 `Host` 별칭. **원격 Claude Code가 도는 호스트**.
- `RemoteDir` — 업로드 위치. `~` 는 원격 `$HOME` 으로 펼쳐짐. `/` 로 시작하면
  절대경로 그대로 사용. 홈 기준이면 어떤 프로젝트에서 Claude를 돌리든 절대경로로
  읽을 수 있어 무난하다. 생략 시 `~/.cursor-drop-files` 기본값.
- **활성 서버**: 파일에 나열된 **첫 번째 서버**가 기본 활성. 우클릭 메뉴에서
  다른 서버를 고르면 활성 전환된다(체크 표시). 이 선택은 **세션 동안만** 유지되며
  ini에 다시 쓰지 않는다 — 재시작하면 첫 서버로 돌아간다.

## 사용

1. `CursorDrop.exe` 더블클릭 → 화면 중앙 알약.
2. (서버가 여럿이면) 위젯 **우클릭 → 서버 선택**으로 활성 서버를 고른다.
3. 파일을 위젯에 **드래그**하거나, 이미지 복사 후 위젯 클릭→**Ctrl+V**
   (또는 위젯 우클릭 → "Paste clipboard").
4. 업로드되고 원격 절대경로가 **클립보드**에 들어간다.
5. WezTerm(원격 Claude Code)에서 **`Ctrl+Shift+V`** 로 붙여 넣으면 끝.

- 위젯 클릭 드래그로 이동, `Esc` 종료, 우클릭 메뉴.
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
| `src/config.rs` | `CursorDrop.ini` 로드/기본생성 + 다중 서버 파서(단위 테스트) |
| `src/clipboard.rs` | 클립보드 파일/비트맵(GDI+ PNG) + 텍스트 설정 |
| `src/upload.rs` | 원격 `$HOME` 해석 + 경로계산 + 클립보드 + scp + 원격 flush(워커 스레드) |
| `src/main.rs` | 윈도/WndProc/우클릭 메뉴/상태머신/입력/CLI |

테스트: `cargo test`.

## Android / Termux 포팅

Windows 알약 위젯의 흐름을 **Android 공유 시트** 기반으로 옮긴 별도 구현.
Rust 크레이트와 코드를 공유하지 않는 **독립 셸 스크립트**다(`android/` 폴더).

> **파일 공유 → Termux** ⇒ 활성 서버로 `scp` 업로드 + 원격 절대경로를 클립보드에
> 복사. Terminus/Termux의 원격 Claude Code 프롬프트에 길게 눌러 붙여 넣는다.

| 파일 | 역할 |
|------|------|
| `cursor-drop.sh` | 전체 흐름: INI 파싱·서버 선택·`~`/`$HOME` 해석·클립보드·`ssh`/`scp` |
| `termux-file-editor` | "**파일** 공유 → Termux" 훅 → 경로로 `cursor-drop.sh` 호출 |
| `termux-url-opener` | "**텍스트/URL** 공유 → Termux" 훅 → stdin의 경로/`content://` URI 처리 |
| `CursorDrop.ini` | 다중 서버 설정 예시 |

### 요구 사항

- **Termux** + **Termux:API** (F-Droid 빌드 권장). `termux-clipboard-set`,
  `termux-toast`, `termux-dialog` 등을 제공.
- 각 서버에 대한 **무암호 SSH 키 인증**. 데스크톱 앱과 동일하게 `BatchMode=yes`
  라서 passphrase 프롬프트가 뜨면 실패한다.

### 설치

```bash
pkg update && pkg install openssh termux-api
mkdir -p ~/bin ~/.config/cursor-drop

cp android/cursor-drop.sh      ~/bin/cursor-drop.sh
cp android/termux-file-editor  ~/bin/termux-file-editor
cp android/termux-url-opener   ~/bin/termux-url-opener
chmod +x ~/bin/cursor-drop.sh ~/bin/termux-file-editor ~/bin/termux-url-opener

cp android/CursorDrop.ini ~/.config/cursor-drop/CursorDrop.ini   # Alias/RemoteDir 수정
```

### 서버 선택

데스크톱 앱과 달리 활성 서버가 **저장**된다(`~/.config/cursor-drop/active`).

```bash
cursor-drop.sh upload a.png # 명시적 업로드(기본 동작과 동일)
cursor-drop.sh list        # 서버 목록, '*' 가 활성
cursor-drop.sh pick        # 라디오 다이얼로그로 선택
cursor-drop.sh use dev     # 이름으로 활성 지정
cursor-drop.sh flush       # 활성 서버 RemoteDir 안의 파일 전부 삭제
```

매 공유마다 대상 서버를 묻고 싶으면(저장된 활성은 안 바뀜) `CURSOR_DROP_PROMPT=1`
설정. 자세한 내용은 `android/README.md` 참고.

## 검증 (myserver 실측)

CLI 모드로 실제 원격 왕복 확인 완료: 기본 ini 생성 → `$HOME` 해석
(`/home/ubuntu`) → 클립보드에 절대경로 → `mkdir`+`touch`+`scp` → 원격 파일
도착(내용 포함) → 정리. scp는 최신 OpenSSH의 SFTP 프로토콜이라 원격 경로를
**따옴표 없이** 전달한다(sanitize로 공백 제거됨).
