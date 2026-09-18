#!/usr/bin/env bash
#
# 통합 빌드 스크립트
#
#   ./scripts/build.sh mac              현재 아키텍처용 .app
#   ./scripts/build.sh mac-universal    Intel + Apple Silicon 통합 .app
#   ./scripts/build.sh win              Windows .exe (크로스 빌드 툴체인 필요)
#   ./scripts/build.sh all              위 전부 (가능한 것만)
#
# 결과물은 dist/ 에 zip 과 함께 놓인다.
set -euo pipefail
cd "$(dirname "$0")/.."

APP_NAME="stratos4 YT Downloader"
BUNDLE_ID="dev.stratos.stratos-dl"
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
DIST="dist"

log()  { printf '\033[1;34m▸\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31m✗\033[0m %s\n' "$*" >&2; exit 1; }

mkdir -p "$DIST"

# ─────────────────────────────────────────────────────────
# macOS
# ─────────────────────────────────────────────────────────
build_mac() {
  local universal="${1:-no}" bin arch_tag
  [[ "$(uname -s)" == "Darwin" ]] || die "macOS 빌드는 macOS 에서만 가능합니다."

  if [[ "$universal" == "universal" ]]; then
    log "macOS 통합 바이너리 빌드 (arm64 + x86_64)"
    rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
    cargo build --release --target aarch64-apple-darwin
    cargo build --release --target x86_64-apple-darwin
    bin="$DIST/stratos-dl-universal"
    lipo -create -output "$bin" \
      target/aarch64-apple-darwin/release/stratos-dl \
      target/x86_64-apple-darwin/release/stratos-dl
    arch_tag="universal"
  else
    log "macOS 빌드 ($(uname -m))"
    cargo build --release
    bin="target/release/stratos-dl"
    arch_tag="$(uname -m)"
  fi

  local app="$DIST/$APP_NAME.app"
  rm -rf "$app"
  mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
  cp "$bin" "$app/Contents/MacOS/stratos-dl"
  chmod +x "$app/Contents/MacOS/stratos-dl"

  # 아이콘 (없으면 기본 아이콘으로 빌드된다. 생성: ./scripts/make-icons.sh)
  local icon_key=""
  if [[ -f img/AppIcon.icns ]]; then
    cp img/AppIcon.icns "$app/Contents/Resources/AppIcon.icns"
    icon_key="  <key>CFBundleIconFile</key>        <string>AppIcon</string>"
  else
    warn "img/AppIcon.icns 가 없습니다 — ./scripts/make-icons.sh 로 생성하세요."
  fi

  cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>            <string>${APP_NAME}</string>
  <key>CFBundleDisplayName</key>     <string>${APP_NAME}</string>
  <key>CFBundleIdentifier</key>      <string>${BUNDLE_ID}</string>
  <key>CFBundleVersion</key>         <string>${VERSION}</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleExecutable</key>      <string>stratos-dl</string>
${icon_key}
  <key>CFBundlePackageType</key>     <string>APPL</string>
  <key>LSMinimumSystemVersion</key>  <string>11.0</string>
  <key>NSHighResolutionCapable</key> <true/>
</dict>
</plist>
PLIST

  # 서명이 없으면 Gatekeeper 가 실행을 막으므로 임시(ad-hoc) 서명을 붙인다.
  # CODESIGN_ID 를 지정하면 정식 서명을 사용한다.
  codesign --force --deep --sign "${CODESIGN_ID:--}" "$app" 2>/dev/null \
    || warn "코드 서명 실패 (무시하고 계속)"

  ( cd "$DIST" && rm -f "stratos-dl-${VERSION}-macos-${arch_tag}.zip" \
    && zip -qry "stratos-dl-${VERSION}-macos-${arch_tag}.zip" "$APP_NAME.app" )
  # .app 안에 복사했으므로 중간 산출물은 남기지 않는다.
  rm -f "$DIST/stratos-dl-universal"
  log "완료: $DIST/stratos-dl-${VERSION}-macos-${arch_tag}.zip"
}

# ─────────────────────────────────────────────────────────
# Windows (크로스 빌드)
# ─────────────────────────────────────────────────────────
detect_win_toolchain() {
  if command -v cargo-xwin >/dev/null 2>&1; then
    echo "xwin"
  elif command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
    echo "mingw"
  else
    echo "none"
  fi
}

build_win() {
  local kind; kind="$(detect_win_toolchain)"
  case "$kind" in
    xwin)
      log "Windows 빌드 (cargo-xwin / MSVC ABI)"
      rustup target add x86_64-pc-windows-msvc >/dev/null
      cargo xwin build --release --target x86_64-pc-windows-msvc
      cp target/x86_64-pc-windows-msvc/release/stratos-dl.exe "$DIST/"
      ;;
    mingw)
      log "Windows 빌드 (mingw-w64 / GNU ABI)"
      rustup target add x86_64-pc-windows-gnu >/dev/null
      cargo build --release --target x86_64-pc-windows-gnu
      cp target/x86_64-pc-windows-gnu/release/stratos-dl.exe "$DIST/"
      ;;
    none)
      warn "Windows 크로스 빌드 툴체인이 없습니다."
      warn "  설치:  ./scripts/setup-cross.sh"
      warn "  또는 태그를 푸시해 GitHub Actions 에서 빌드하세요."
      return 1
      ;;
  esac
  ( cd "$DIST" && rm -f "stratos-dl-${VERSION}-windows-x64.zip" \
    && zip -qj "stratos-dl-${VERSION}-windows-x64.zip" stratos-dl.exe )
  log "완료: $DIST/stratos-dl-${VERSION}-windows-x64.zip"
}

case "${1:-mac}" in
  mac)           build_mac ;;
  mac-universal) build_mac universal ;;
  win)           build_win ;;
  all)
    if [[ "$(uname -s)" == "Darwin" ]]; then build_mac universal; fi
    build_win || true
    ;;
  *) die "알 수 없는 대상: $1  (mac | mac-universal | win | all)" ;;
esac

log "dist/ 내용:"
ls -lh "$DIST" | tail -n +2
