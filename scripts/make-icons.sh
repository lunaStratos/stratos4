#!/usr/bin/env bash
#
# img/icon.png 하나로 플랫폼별 아이콘을 생성한다.
#
#   ./scripts/make-icons.sh
#
#   → img/AppIcon.icns   macOS .app 번들용 (iconutil, macOS 에서만 생성 가능)
#   → img/icon.ico       Windows .exe 리소스용 (PNG 내장 ICO, Win7+)
#
# 생성 결과는 저장소에 커밋한다. CI 러너는 이 스크립트를 돌리지 않고
# 커밋된 파일을 그대로 사용한다.
set -euo pipefail
cd "$(dirname "$0")/.."

SRC="img/icon.png"
[[ -f "$SRC" ]] || { echo "✗ $SRC 가 없습니다." >&2; exit 1; }

command -v sips >/dev/null || { echo "✗ sips 가 필요합니다 (macOS)." >&2; exit 1; }

SRC_W="$(sips -g pixelWidth "$SRC" | awk '/pixelWidth/{print $2}')"
if (( SRC_W < 1024 )); then
  printf '\033[1;33m!\033[0m 원본이 %spx 입니다. 큰 크기는 확대되어 흐려집니다 (권장: 1024px).\n' "$SRC_W" >&2
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

resize() { sips -s format png -z "$1" "$1" "$SRC" --out "$2" >/dev/null; }

# ── macOS .icns ───────────────────────────────────────────
ICONSET="$TMP/AppIcon.iconset"
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
  resize "$s"          "$ICONSET/icon_${s}x${s}.png"
  resize "$((s * 2))"  "$ICONSET/icon_${s}x${s}@2x.png"
done
iconutil -c icns "$ICONSET" -o img/AppIcon.icns
echo "▸ img/AppIcon.icns"

# ── Windows .ico ──────────────────────────────────────────
# ICO 디렉토리에 PNG 를 그대로 담는다 (Vista 이상에서 지원).
for s in 16 32 48 64 128 256; do resize "$s" "$TMP/$s.png"; done
python3 - "$TMP" img/icon.ico <<'PY'
import struct, sys, pathlib
tmp, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
sizes = [16, 32, 48, 64, 128, 256]
blobs = [(s, (tmp / f"{s}.png").read_bytes()) for s in sizes]

header = struct.pack("<HHH", 0, 1, len(blobs))          # reserved, type=icon, count
offset = len(header) + 16 * len(blobs)
entries, body = b"", b""
for s, data in blobs:
    dim = 0 if s >= 256 else s                          # 256 은 0 으로 표기한다
    entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
    body += data
    offset += len(data)
out.write_bytes(header + entries + body)
PY
echo "▸ img/icon.ico"
