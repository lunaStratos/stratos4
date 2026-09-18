#!/usr/bin/env bash
#
# macOS 에서 Windows 실행 파일을 만들기 위한 크로스 빌드 툴체인 설치.
# 기본은 cargo-xwin (MSVC ABI — GitHub Actions 결과물과 동일한 형태).
#   ./scripts/setup-cross.sh          cargo-xwin 방식
#   ./scripts/setup-cross.sh mingw    mingw-w64 방식 (GNU ABI)
set -euo pipefail

log() { printf '\033[1;34m▸\033[0m %s\n' "$*"; }

case "${1:-xwin}" in
  xwin)
    log "LLVM(lld) 설치 — 링커로 사용"
    command -v brew >/dev/null || { echo "Homebrew 가 필요합니다."; exit 1; }
    brew list --formula llvm >/dev/null 2>&1 || brew install llvm
    log "cargo-xwin 설치"
    cargo install --locked cargo-xwin
    log "Rust 타깃 추가"
    rustup target add x86_64-pc-windows-msvc
    log "완료 — 이제 'make win' 또는 './scripts/build.sh win' 이 동작합니다."
    ;;
  mingw)
    log "mingw-w64 설치"
    command -v brew >/dev/null || { echo "Homebrew 가 필요합니다."; exit 1; }
    brew list --formula mingw-w64 >/dev/null 2>&1 || brew install mingw-w64
    log "Rust 타깃 추가"
    rustup target add x86_64-pc-windows-gnu
    log "완료 — 이제 'make win' 또는 './scripts/build.sh win' 이 동작합니다."
    ;;
  *) echo "사용법: $0 [xwin|mingw]"; exit 1 ;;
esac
