#!/usr/bin/env bash
# TPM entry point. Builds the arbor binary on first use and after updates,
# then hands over to `arbor init`, which sets up bindings and hooks.

CURRENT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="$CURRENT_DIR/target/release/arbor"
STAMP="$CURRENT_DIR/target/release/.arbor-rev"

revision() {
	git -C "$CURRENT_DIR" rev-parse HEAD 2>/dev/null || echo unknown
}

find_cargo() {
	command -v cargo 2>/dev/null || { [ -x "$HOME/.cargo/bin/cargo" ] && echo "$HOME/.cargo/bin/cargo"; }
}

build_and_init() {
	local cargo="$1"
	tmux display-message "tmux-arbor: building…"
	if (cd "$CURRENT_DIR" && "$cargo" build --release --quiet); then
		revision >"$STAMP"
		"$BIN" init
		tmux display-message "tmux-arbor: ready"
	else
		tmux display-message "tmux-arbor: cargo build failed in $CURRENT_DIR"
	fi
}

main() {
	local cargo
	cargo="$(find_cargo)"
	if [ -x "$BIN" ] && [ "$(cat "$STAMP" 2>/dev/null)" = "$(revision)" ]; then
		"$BIN" init
	elif [ -n "$cargo" ]; then
		# Keep using an older build while the new one compiles; building
		# can take a minute and must not hold up tmux.
		[ -x "$BIN" ] && "$BIN" init
		build_and_init "$cargo" </dev/null >/dev/null 2>&1 &
		disown
	elif command -v arbor >/dev/null 2>&1; then
		arbor init
	else
		tmux display-message "tmux-arbor: install Rust (cargo) to build it, or put an arbor binary in PATH"
	fi
}

main
