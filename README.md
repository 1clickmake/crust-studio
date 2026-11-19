# CRust Studio

디자이너와 개발자를 위한 **CRust** 원클릭 로컬 런처 & 테마 스튜디오 (Tauri 2 + Rust).

## 주요 기능
- **원클릭 서버 실행/중지**: 로컬 포트 체크 및 `dev_mode = true` 간편 전환
- **실시간 브라우저 열기**: 사이트 홈, 관리자(`/admin`), 테마 미리보기(`?preview_theme=...`) 즉시 실행
- **테마 스튜디오**: `default` 테마 기반 새 테마 복제 생성, 폴더 바로 열기, 판매·배포용 `.zip` 패키징

## 개발 실행 방법
```bash
cd src-tauri
cargo run
```
릴리즈 빌드:
```bash
cd src-tauri
cargo build --release
```
빌드된 `src-tauri/target/release/crust-studio.exe` 옆에 `runtime/` (또는 `crust/`) 폴더로 CRust 실행 파일과 `templates/`, `static/` 을 두면 포터블 원클릭 런처로 바로 배포할 수 있습니다.

## Windows + macOS 동시 자동 빌드 (GitHub Actions)
`.github/workflows/release.yml` 이 포함되어 있어, GitHub 저장소에 푸시한 뒤 Actions 탭에서 **Run workflow** 를 누르거나 `v*` 태그를 푸시하면 아래 3가지 포터블 패키지가 자동으로 빌드됩니다.
- `CRust-Studio-Windows-x64` (`crust-studio.exe` + `runtime/`)
- `CRust-Studio-macOS-AppleSilicon` (`CRust Studio.app` + `runtime/` — M1/M2/M3/M4)
- `CRust-Studio-macOS-Intel` (`CRust Studio.app` + `runtime/`)

> **macOS 최초 실행 안내 (디자이너용)**: Apple 개발자 유료 서명이 없는 빌드이므로, 처음 실행할 때는 더블클릭 대신 **앱 아이콘 우클릭 → 열기** 를 한 번만 눌러주시면 정상 실행됩니다.
