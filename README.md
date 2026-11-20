# rrredis

A Redis-compatible server written in Rust, built step by step as a way to learn the language.

This is a learning project, not a production server. The code follows a 27-task curriculum in [TASKS.md](TASKS.md). Each task adds one Redis feature and introduces the Rust concepts it needs, from a TCP echo server up to async I/O, persistence, and transactions.

## Status

The server currently speaks a plain-text line protocol, not RESP yet.

| Phase | Tasks | State |
|---|---|---|
| A. Foundations | 1–4 | Tasks 1–3 done: threaded TCP server, line protocol, line length cap. Task 4 (RESP values) is next. |
| B–G. Protocol through async and persistence | 5–27 | Not started |

What works today, on `127.0.0.1:6380`:

| Input (one per line, any case) | Reply |
|---|---|
| `PING` | `+PONG\r\n` |
| `ECHO <text>` | `<text>\r\n` |
| anything else, including an empty line | `-ERR unknown command\r\n` |
| a line that is not valid UTF-8 | `-ERR invalid utf-8\r\n`, and the connection stays open |
| a line longer than 1 MiB | `-ERR line too long\r\n`, then the connection is closed gracefully |

The server uses port 6380, so a real Redis on 6379 can run next to it for comparison.

## Requirements

- Rust 1.85 or newer, installed with [rustup](https://rustup.rs). The project uses edition 2024.
- `nc` for manual checks.
- `redis-cli` from Task 6 onward (`brew install redis`).

The project has no external crates yet. The curriculum introduces the first one in Task 17.

## Run it

```sh
cargo run
```

In another terminal:

```sh
$ nc 127.0.0.1 6380
PING
+PONG
echo hi
hi
FOO
-ERR unknown command
```

## Test it

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The tests start the server on an ephemeral port, so they don't conflict with a running instance. They cover:

- concurrent clients
- clients that disconnect or half-close mid-session
- the reply table for each command
- the line-length boundary (exactly 1 MiB accepted, one byte more rejected)
- invalid UTF-8

## Project layout

```
src/main.rs    the whole server so far: reply(), handle(), serve(), and the tests
TASKS.md       the 27-task curriculum
TESTS.md       recommended tests per task
REFERENCES.md  where to look when stuck, organised by symptom and by task
SOLUTIONS.md   worked implementations of the tricky parts (spoilers)
CLAUDE.md      ground rules for using Claude Code on this repo
```

The curriculum suggests splitting the code into modules (`resp`, `command`, `db`, `server`) from Task 8 onward.

## How the work is organised

- Each task ends in a runnable state: `cargo build` is warning-free, `cargo test` passes, and the task's checkpoint works against `nc` or `redis-cli`.
- Each task is committed when it is finished.
- Design questions are settled against real Redis. Run `redis-server --port 6379` and compare replies from both servers.

## Using Claude Code here

[CLAUDE.md](CLAUDE.md) tells Claude to teach, not to write the project. It explains, reviews code, and says which tests are needed. It shows only short snippets, and only when asked. It does not edit source files.

## References

- [The Rust Book](https://doc.rust-lang.org/book/)
- [RESP protocol spec](https://redis.io/docs/latest/develop/reference/protocol-spec/)
- [Redis commands](https://redis.io/docs/latest/commands/)
- [rsedis](https://github.com/seppo0010/rsedis), an older Redis clone in Rust, used as a structural reference

More links, grouped by task, are in [REFERENCES.md](REFERENCES.md).
