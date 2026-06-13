# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

WezTerm is a GPU-accelerated, cross-platform terminal emulator and multiplexer written in Rust. User-facing docs live at https://wezterm.org/ (source in `docs/`).

## Build, test, lint

This is a large Cargo workspace. Use the `Makefile` targets (or the equivalent `justfile` recipes — see below), which encode the canonical invocations:

- `make build` — builds the four shipped binaries: `wezterm` (CLI/client), `wezterm-gui`, `wezterm-mux-server`, `strip-ansi-escapes`.
- `make check` / `cargo check` — fast type-check loop; preferred for iterating before a full build.
- `make test` — runs `cargo nextest run`, then re-runs `-p wezterm-escape-parser` separately because that crate is `no_std` by default.

The repo also ships a `justfile` mirroring these as `just`-friendly recipes (`just check`, `just build`, `just test`, `just fmt`, `just run`, `just clippy`, …). Run `just --list` to see them. There are also `just test-one <name>` for a single test and Nix recipes (`just nix-build`, `just nix-shell`).

Run a single test: `cargo nextest run -p <crate> <test_name_substring>` (or `cargo test -p <crate> <name>` if nextest isn't installed).

First-time setup needs system libraries and git submodules:
- `git submodule update --init --recursive` (vendored harfbuzz, freetype, libpng, zlib under `deps/`).
- `./get-deps` installs OS packages (translate new OS-specific install steps into this script rather than into docs).

On Nix/NixOS, skip `./get-deps` and use the pinned dev shell instead: `nix develop 'git+file:.?dir=nix'` (or `just nix-shell`). The flake under `nix/` provides pkg-config and the X11/Wayland/xcb/font dev libraries; building outside this shell fails to find them (`x11.pc`, `openssl`, etc.). `just nix-build` / `just nix-build-headless` build the GUI / headless packages via the flake.

Docs: `ci/build-docs.sh serve` builds and live-reloads the site locally.

## Architecture

The workspace is layered from low-level terminal protocol up to the GPU GUI. The key insight is the **separation between the terminal model and the windowing/rendering**, with a **multiplexer** layer in between that lets the same terminal panes be served locally, over SSH, over TLS, or attached to tmux.

### Terminal model (windowing-agnostic)
- **`wezterm-escape-parser`, `vtparse`** — parse incoming bytes / escape sequences into actions. `vtparse` is the state machine; `wezterm-escape-parser` turns it into typed actions.
- **`term/`** (crate `wezterm-term`) — the core terminal emulation: cells, `screen.rs`, and `terminalstate/` (the bulk of escape-sequence handling). This is independent of any windowing system. **Escape-sequence behavior and xterm compatibility work belongs here** (reference: https://invisible-island.net/xterm/ctlseqs/ctlseqs.html).
- **`wezterm-cell`, `wezterm-surface`, `term/screen.rs`** — cell/grid/surface data structures.
- **`termwiz/`** — standalone terminal toolkit (capabilities, input parsing, line editing, widgets, rendering). Usable independently of wezterm; also powers CLI rendering.

### Multiplexer
- **`mux/`** — the heart of session management. Core abstractions:
  - **Pane** (`pane.rs`, `localpane.rs`) — a single terminal instance backed by a PTY.
  - **Tab** (`tab.rs`) — a layout of split panes.
  - **Window** (`window.rs`) — a collection of tabs.
  - **Domain** (`domain.rs`) — *where/how* panes are spawned: local process, SSH (`ssh.rs`), tmux (`tmux*.rs`), or remote mux. New connection types implement the `Domain` trait.
- **`codec/`** — the wire protocol (serde-based PDUs) spoken between the `wezterm` client and `wezterm-mux-server`.
- **`wezterm-mux-server*`** — headless server hosting a mux; `wezterm-client/` is the client side. `wezterm-mux-server-impl/` holds dispatch/session handling and PKI for TLS domains.
- **`wezterm-ssh/`, `wezterm-uds/`, `async_ossl`** — transport plumbing (SSH, unix domain sockets, OpenSSL async).

### Windowing & GPU
- **`window/`** — cross-platform window/GL/GPU abstraction. Per-OS backends in `window/src/os/` (X11, Wayland, macOS, Windows). Owns EGL/WebGPU context creation.
- **`wezterm-gui/`** — the GUI frontend and renderer. `termwindow/` is the central GUI object (input handling in `keyevent.rs`/`mouseevent.rs`, rendering in `termwindow/render/`, overlays/modals like char-select and pane-select). Glyph rasterization/caching in `glyphcache.rs`/`shapecache.rs`/`customglyph.rs`; rendering uses both an OpenGL path (`*.glsl`) and a WebGPU path (`shader.wgsl`, `webgpu.rs`). `commands.rs`/`inputmap.rs` map key/mouse assignments to actions.
- **`wezterm-font/`** — font discovery, fallback, and shaping (harfbuzz/freetype/fontconfig via vendored `deps/`).

### Config & Lua
- **`config/`** — the configuration model. WezTerm is configured in **Lua** (`config/src/lua.rs`); `keyassignment.rs`/`keys.rs` define key/mouse actions.
- **`lua-api-crates/`** — the Lua API surface, split into many small crates (battery, color-funcs, mux, window-funcs, etc.) deliberately to keep build times down. They are registered into the Lua runtime via **`env-bootstrap/`**, which is also where global process/environment setup happens. To expose new functionality to user config, add or extend one of these crates and wire it through `env-bootstrap`.

### Binaries (entry points)
- `wezterm/src/main.rs` — the `wezterm` CLI: multiplexer client, `cli` subcommands, and launcher.
- `wezterm-gui/src/main.rs` — the GUI application.
- `wezterm-mux-server/` — the headless server.

## Conventions

- `wezterm-dynamic` provides a `Value` type and derive macros used pervasively for Lua<->Rust conversion and serialization; prefer it over ad-hoc conversions when bridging config/Lua.
- `wezterm-escape-parser` and several leaf crates are `no_std`-compatible — don't introduce `std`-only deps there without checking `default-features`.
- Add tests for terminal behavior under `term/src/test/`; there are helpers for asserting rendered screen contents.
- Many `deps/*` crates are vendored C libraries built via `cc`/submodules; avoid editing vendored sources.
