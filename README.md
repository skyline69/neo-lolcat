# neo-lolcat

Rust reimplementation of [lolcat](https://github.com/busyloop/lolcat) maintained by [skyline69](https://github.com/skyline69). It sticks to the standard library while matching the original help surface.

## Features

- Streams stdin or multiple files (treating `-` as stdin) with the familiar rainbow gradient.
- Supports the classic flags (`--spread`, `--freq`, `--seed`, `--animate`, etc.) plus a `--debug` mode for diagnostics.
- Detects truecolor terminals automatically while allowing explicit `--truecolor`/`--force` overrides.
- Handles `Broken pipe`/`SIGPIPE` situations gracefully so pipelines like `macchina | lolcat -t --animate 1 | head -n1` exit cleanly.
- Includes unit and integration tests to lock in parser behavior and CLI regressions.

## Building

```bash
cargo build --release
```

Optimized binaries are emitted to `target/release/lolcat`. The release profile enables `opt-level=3`, fat LTO, single codegen unit, and `panic = "abort"` for maximum throughput.

## Testing

```bash
cargo fmt
cargo test
cargo clippy -- -D warnings
```

The integration suite under `tests/cli.rs` spawns the compiled binary and checks help/version output plus forced-color pipelines; `tests/stress.rs` feeds random binary data to guard against crashes. For longer runs, `scripts/stress.sh` streams configurable amounts of random data through the release binary (requires Python 3).

### Continuous Integration

A GitHub Actions workflow (`.github/workflows/ci.yml`) runs fmt, clippy, unit + integration tests, and a release build on every push/PR to keep the binary stable across toolchains.

## Usage

```
Usage: lolcat [OPTION]... [FILE]...

Concatenate FILE(s), or standard input, to standard output.
With no FILE, or when FILE is -, read standard input.
```

Refer to `lolcat --help` for the complete flag list and examples. Use `--debug` or the legacy `LOLCAT_DEBUG=1` environment variable to see internal diagnostics when troubleshooting terminal quirks.
