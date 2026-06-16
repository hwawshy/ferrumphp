# FerrumPHP

A PHP application server written in Rust. Embeds the Zend engine to serve HTTP requests through a custom SAPI. Intended to run behind a reverse proxy rather than being a full-fledged web server.

Designed to server PHP applications written using the front controller pattern.

> **Early development** — not yet production-ready.

## Quick start

Right now the fastest way is to build and run the docker image using the provided Dockerfile

```sh
docker build -t ferrumphp .
```

```sh
docker run -v "$PWD":/app -p 8080:8080 ferrumphp --entrypoint /app/public/index.php --bind 0.0.0.0:8080
```

| Flag | Default | Description |
|---|---|---|
| `--entrypoint` | — | PHP front controller (required) |
| `--bind` | `127.0.0.1:8080` | Address to bind |
| `--workers` | `10` | Number of PHP worker threads |
| `--trusted-proxy` | — | Trusted proxy CIDR (repeatable) |

## Architecture

- **One OS thread per PHP worker** — workers receive jobs over `crossbeam_channel`
- **Tower Service** with tracing and request-id middleware
- **Request/response body streaming** through tokio mpsc channels
- **Proxy support** — `X-Forwarded-For` with configurable trusted proxy CIDRs
