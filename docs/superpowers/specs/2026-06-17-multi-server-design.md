# 다중 서버 지원 설계

날짜: 2026-06-17

## 목표

CursorDrop이 여러 원격 서버를 알고 있고, 우클릭 메뉴에서 **활성 서버 1개**를
전환할 수 있게 한다. 이후 모든 업로드는 활성 서버로만 전송된다.

## 결정 사항

- **선택 의미**: 활성 서버 1개 전환 (동시 다중 업로드 아님).
- **선택 유지**: 세션 메모리만. ini에 기록하지 않음. 앱 재시작 시 첫 서버로 복귀.
- **ini 포맷**: 서버별 `[Server:<name>]` 섹션. 섹션 이름이 메뉴 라벨.

## INI 포맷

```ini
; CursorDrop config — 여러 서버를 [Server:이름] 으로 나열.
; 우클릭 메뉴에서 활성 서버를 전환. 첫 번째 서버가 기본 활성.
;   Alias     = ~/.ssh/config 의 Host 별칭
;   RemoteDir = 업로드 위치. '~' 는 원격 $HOME 으로 펼쳐짐. '/' 시작은 절대경로.
[Server:prod]
Alias=myserver
RemoteDir=~/.cursor-drop-files

;[Server:dev]
;Alias=devbox
;RemoteDir=~/uploads
```

- **하위호환**: 기존 `[Remote]` 섹션도 새 서버 시작으로 인식. 이름 `Remote`인
  서버 하나로 취급. 기존 ini 사용자는 변경 없이 동작.
- 파일 내 **첫 번째 서버 = 기본 활성**.
- 서버가 하나도 파싱되지 않으면 기본 서버 1개(`myserver` / `~/.cursor-drop-files`)로 폴백.
- 첫 실행 시 위 형식의 기본 ini 생성(둘째 서버는 주석).

## 컴포넌트 변경

### config.rs
```rust
pub struct Server { pub name: String, pub alias: String, pub remote_dir: String }
pub fn load() -> Vec<Server>   // 항상 최소 1개 반환
```
- 섹션 헤더(`[...]`)를 인식하는 파서로 확장.
- `[Server:<name>]` → 새 서버 시작, name = `<name>`.
- `[Remote]` → 새 서버 시작, name = `Remote` (하위호환).
- 각 서버 아래 `Alias` / `RemoteDir` 키 수집.
- 단위 테스트: 다중 서버, `[Remote]` 호환, 빈/누락 → 기본 1개.

### main.rs
- 시작 시 `config::load()` 1회 → 전역 `OnceLock<Vec<Server>>`.
- 활성 인덱스 `AtomicUsize`(기본 0). 세션 메모리만.
- 활성 서버 접근 헬퍼 제공(워커 스레드에서 호출).

### 우클릭 메뉴 (`show_menu`)
```
● prod        ← 라디오 체크 = 현재 활성
  dev
─────────
Paste clipboard
─────────
Show log
─────────
Exit
```
- 서버 항목 커맨드 ID = `ID_SERVER_BASE(200) + index`.
- 활성 항목에 `MF_CHECKED`(라디오 스타일) 표시.
- 서버 클릭 → 활성 인덱스만 변경(업로드 안 함).
- 서버가 1개뿐이면 서버 목록/구분선 생략(기존 모습 유지).

### upload.rs
- `run()`/`handle_files()`가 매번 `config::load()` 하던 것을 활성 서버를 인자로
  받도록 변경: `run(files, &Server)`.
- 호출부(드롭/페이스트)는 활성 서버를 읽어 전달.
- CLI 모드는 활성(=첫) 서버 사용.

## 에러 처리

- 서버 파싱 0개 → 기본 서버 폴백(기존 동작 유지).
- 활성 인덱스 범위 밖(이론상 불가) → 0으로 클램프.
- 원격 도달 실패 등 기존 에러 상태 머신 그대로.

## 테스트

- `config.rs` 단위 테스트(파서).
- `cargo build` / `cargo test` 통과.
- Windows 전용 GUI 부분은 빌드 확인 위주(런타임은 수동).
