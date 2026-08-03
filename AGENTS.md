# AGENTS.md

Rust (edition 2024) PHP application server that embeds the Zend engine via `ext-php-rs` and serves HTTP through a custom SAPI (see README).

## Building

The build embeds a **thread-safe (ZTS) PHP** and links `dylib=php`. `build.rs` compiles `build/ferrumphp.c` with clang and runs bindgen against `build/ferrumphp.h`, requiring `php-config` (with `--embed` support), clang, and libclang.

The host machine's PHP is 8.4 NTS — a plain `cargo build` will fail. Use one of:

- `nix develop` (preferred): flake provides PHP 8.5 ZTS+embed, clang, libclang, and sets `LIBCLANG_PATH`, `BINDGEN_EXTRA_CLANG_ARGS`, `PHP_INI_SCAN_DIR`. Then `cargo build` / `cargo run -- --entrypoint /path/to/app.php`.
- Docker: `docker build -t ferrumphp .` (uses `php:8.5-zts-trixie`; sets `LIBRARY_PATH=/usr/local/lib` for the php dylib).

No tests, CI, linters, or formatter config exist. Verify changes with `cargo build` (in the flake shell).

## Architecture

- `src/main.rs`: tokio/hyper server. Per-connection `PhpService` (tower) sends `Job`s over a tokio mpsc channel to a "SAPI worker" thread, which fans out over a crossbeam channel to `--workers` PHP worker threads.
- `src/php/mod.rs`: `WorkerPool` + `WorkerSupervisor` (restarts a worker on error; kills pool when the job channel closes).
- `src/php/worker.rs` + `context.rs`: each worker owns a `WorkerContext` (a PHP interpreter); interpreter state is not shared across workers.
- `src/php/sapi.rs` + `ffi.rs` + `build/ferrumphp.c`/`.h`: the custom SAPI and hand-written C bridge to the Zend engine. The bindgen allowlist is in `build.rs`.
- `src/cli.rs`: clap CLI (`--entrypoint` required, `.php` file; `--bind`, `--workers`, `--trusted-proxy`). Validated config is cached in the `CONFIG` `OnceLock` global (`src/main.rs:22`).
- `src/php/index.php`: scratch test script, not part of the runtime.

## Gotchas

- After touching `build/ferrumphp.{c,h}`, re-run `cargo build` (it is re-run automatically via `rerun-if-changed`).
- PHP's ZTS build means the C code must be compiled with `-DZTS=1`; keep it consistent in `build.rs`.
- The server is meant to run behind a reverse proxy; trust proxy CIDRs via `--trusted-proxy`.
