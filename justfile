# wezterm common tasks. Run `just` or `just --list` to see recipes.

# Show available recipes
default:
    @just --list

# Type-check the workspace (fast iteration loop)
check:
    cargo check
    cargo check -p wezterm-escape-parser
    cargo check -p wezterm-cell
    cargo check -p wezterm-surface
    cargo check -p wezterm-ssh

# Build the shipped binaries (debug). Pass --release via `just build --release`
build *ARGS:
    cargo build {{ARGS}} -p wezterm
    cargo build {{ARGS}} -p wezterm-gui
    cargo build {{ARGS}} -p wezterm-mux-server
    cargo build {{ARGS}} -p strip-ansi-escapes

# Release build of all shipped binaries
release: (build "--release")

# Run the test suite (uses nextest; escape-parser is no_std so runs separately)
test:
    cargo nextest run
    cargo nextest run -p wezterm-escape-parser

# Run a single test by name substring, e.g. `just test-one csi -p wezterm-term`
test-one NAME *ARGS:
    cargo nextest run {{ARGS}} {{NAME}}

# Lint with clippy across the workspace
clippy:
    cargo clippy --all-targets

# Format all code (requires the nightly rustfmt — imports_granularity is nightly-only)
fmt:
    cargo +nightly fmt

# Check formatting without modifying files (what CI enforces)
fmt-check:
    cargo +nightly fmt --check

# Run the GUI in debug mode
run *ARGS:
    cargo run -p wezterm-gui {{ARGS}}

# Build the documentation site
docs:
    ci/build-docs.sh

# Serve docs locally with live reload
servedocs:
    ci/build-docs.sh serve

# Initialize/refresh vendored C submodules (harfbuzz, freetype, libpng, zlib)
submodules:
    git submodule update --init --recursive

# Install OS package dependencies
deps:
    ./get-deps

# Build the GUI package via the Nix flake (lives in nix/)
nix-build:
    nix build '.?dir=nix'

# Build the headless (wezterm + mux-server) Nix package
nix-build-headless:
    nix build '.?dir=nix#default.headless'

# Enter the Nix dev shell with the pinned toolchain
nix-shell:
    nix develop '.?dir=nix'

# Remove build artifacts
clean:
    cargo clean
