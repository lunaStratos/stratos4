# stratos4 YT Downloader 빌드 자동화
#
#   make            개발 빌드 후 실행
#   make dev        개발 빌드 후 실행 (= make)
#   make build      릴리스 빌드
#   make mac        macOS .app + zip (현재 아키텍처)
#   make mac-universal   Intel + Apple Silicon 통합 .app + zip
#   make win        Windows .exe + zip (크로스 툴체인 필요)
#   make dist       가능한 모든 플랫폼 패키징
#   make check      fmt + clippy + 빌드 검사
#   make icons      img/icon.png → .icns / .ico 재생성
#   make tools      yt-dlp / ffmpeg 최신판 설치
#   make status     yt-dlp / ffmpeg 설치 상태
#   make clean      target/ 과 dist/ 정리

SHELL := /bin/bash
BIN   := stratos-dl

.DEFAULT_GOAL := dev
.PHONY: dev build run mac mac-universal win dist check fmt clippy test icons tools status setup-cross clean

dev:
	cargo run

build:
	cargo build --release

run: build
	./target/release/$(BIN)

mac:
	./scripts/build.sh mac

mac-universal:
	./scripts/build.sh mac-universal

win:
	./scripts/build.sh win

dist:
	./scripts/build.sh all

check: fmt clippy
	cargo build --release

fmt:
	cargo fmt --all -- --check || (echo "→ 'cargo fmt --all' 로 정리하세요"; exit 1)

clippy:
	cargo clippy --release --all-targets -- -D warnings

test:
	cargo test

icons:
	./scripts/make-icons.sh

tools: build
	./target/release/$(BIN) --install-tools

status: build
	./target/release/$(BIN) --check-tools

setup-cross:
	./scripts/setup-cross.sh

clean:
	cargo clean
	rm -rf dist
