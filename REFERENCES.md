# References: when you're stuck

Use this file by symptom first (section 1), then by task (section 2). Try for 20–30 minutes before looking at rsedis. Read, close the tab, and write your own version, so you absorb the idea rather than the syntax.

## 0. Core links

| Resource | Use it for |
|---|---|
| [The Rust Book](https://doc.rust-lang.org/book/) | The main text. Read the chapter named in each task. |
| [Rust by Example](https://doc.rust-lang.org/rust-by-example/) | Short runnable snippets when the Book is too verbose. |
| [std docs](https://doc.rust-lang.org/std/) | Every collection, trait and I/O type. Use the search box (`s` key). |
| [Rust error index](https://doc.rust-lang.org/error_codes/) | Look up any `E0xxx` code from the compiler. Each has examples. |
| [Rustlings](https://github.com/rust-lang/rustlings) | Optional drills if a concept (e.g. lifetimes) isn't sticking. |
| [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/) | Naming and design conventions to apply in refactor tasks. |
| [Tokio tutorial](https://tokio.rs/tokio/tutorial) | Tasks 22–23. It builds mini-redis. |
| [Redis protocol spec (RESP)](https://redis.io/docs/latest/develop/reference/protocol-spec/) | Tasks 4–6. |
| [Redis commands](https://redis.io/docs/latest/commands/) | Exact semantics, error messages and edge cases per command. |
| [rsedis](https://github.com/seppo0010/rsedis) | Structural reference (see section 3). |
| [SOLUTIONS.md](SOLUTIONS.md) | Tested implementations of the hard parts, when hints are not enough. |
| [Too Many Lists](https://rust-unofficial.github.io/too-many-lists/) | Optional. Ownership and `unsafe` in depth (Task 27). |
| [Rustonomicon](https://doc.rust-lang.org/nomicon/) | Only for the `unsafe` stretch goal. |
| [docs.rs](https://docs.rs) | Docs for every crate (`clap`, `thiserror`, `tokio`, `bytes`, `criterion`, ...). |

Bring your own real Redis for differential testing: `redis-server --port 6379` and compare replies from `redis-cli -p 6379` and `redis-cli -p 6380`. Real Redis is the spec when the docs are ambiguous.

## 1. By symptom

### Compiler errors you will meet

| Error | What it usually means | Typical fix | Book chapter |
|---|---|---|---|
| `E0382 borrow of moved value` | You used a value after passing it by value. | Pass `&x`, `.clone()` deliberately, or restructure so ownership flows one way. | Ch. 4 Ownership |
| `E0499 cannot borrow as mutable more than once` | Two live `&mut` to the same thing. | Shorten the first borrow's scope, split the struct into fields, or collect results before mutating. | Ch. 4 |
| `E0502 cannot borrow as mutable because also borrowed as immutable` | You hold a `&` from `map.get()` and then call `map.insert()`. | Copy/clone out what you need, end the borrow, then mutate. Or use `entry()`. | Ch. 4 |
| `E0106 missing lifetime specifier` | A function returns a reference and the compiler can't tell which input it borrows from. | Add `<'a>` to tie them together, or return an owned value. | Ch. 10.3 |
| `E0597 borrowed value does not live long enough` | A reference outlives its owner (often in a thread closure). | `move` the owned value, or use `Arc`. | Ch. 10.3, 16 |
| `E0373 closure may outlive the current function` | `thread::spawn` closure captured a local by reference. | Add `move`, and `Arc::clone` before the closure. | Ch. 16.1 |
| `E0277 ... cannot be sent between threads safely` | A `!Send` type (like `Rc`, or a `MutexGuard` across `.await`) crossed a thread boundary. | Use `Arc`; drop guards before `.await`. | Ch. 16.4 |
| `E0308 mismatched types` (`&String` vs `&str`, `Vec<u8>` vs `&[u8]`) | Owned vs borrowed variants. | `&s[..]`, `.as_slice()`, `.as_bytes()`, `.to_vec()`, `String::from_utf8`. | Ch. 4.3, 8.2 |
| `E0599 no method named ... found` | Trait not in scope. | `use std::io::{Read, Write, BufRead};`. | Ch. 10.2 |
| `E0507 cannot move out of borrowed content` | You tried to take ownership through a reference. | `.clone()`, `Option::take()`, `std::mem::take`, or match on `&`. | Ch. 4, 6 |
| `E0004 non-exhaustive patterns` | A new enum variant isn't handled. | This is the compiler helping. Add the arm. | Ch. 6.2 |
| `E0282 type annotations needed` | `collect()`, `parse()` or `into()` can't infer the target. | `let v: Vec<_> = ...` or turbofish `parse::<i64>()`. | Ch. 3, 13 |
| `the trait bound ... is not satisfied` on a `HashMap` key | Key type lacks `Hash + Eq`. | `#[derive(Hash, PartialEq, Eq)]`. `f64` can't be a key. | Ch. 8.3 |

### Design questions you will hit

| Question | Where to look |
|---|---|
| "Should this take `&str`, `String`, or `impl Into<String>`?" | API Guidelines (C-CALLER-CONTROL); rule of thumb: borrow in parameters unless you need to store. |
| "`&[u8]` or `&Vec<u8>`?" | Always `&[u8]` (clippy `ptr_arg`). |
| "I need a struct that holds a reference." | Usually don't. Own the data. Self-referential structs are a known dead end; restructure. |
| "`Rc<RefCell<T>>` everywhere?" | A smell here. This project is `Arc<Mutex<T>>` or message passing. Book Ch. 15 and 16. |
| "Enum vs trait object?" | Enum when the set of variants is closed (RESP types, commands, db values). Trait objects when open to extension. |
| "Can I `unwrap` here?" | Only for provable invariants, ideally with `expect("why")`. |
| "My `match` is huge." | Extract per-command parse and execute functions; keep the top-level match as a dispatch table only. |
| "`clone()` to make the borrow checker quiet?" | OK to unblock yourself; leave a `// TODO(clone)` and revisit in Task 26. |
| "Deadlock or hang with `Mutex`." | You hold a guard while calling something that locks again, or while blocking on I/O. Print where the guard is created and dropped; use `drop(guard)`. |

### Tooling

```sh
cargo check              # fastest feedback, no codegen
cargo clippy -- -W clippy::pedantic   # stricter lessons (ignore noisy lints)
cargo fmt
cargo test -- --nocapture            # see println! in tests
cargo test name -- --exact
RUST_BACKTRACE=1 cargo run
RUST_LOG=debug cargo run             # after Task 19
cargo expand                          # (cargo install cargo-expand) see what derive macros produce
cargo tree -d                         # duplicate dependency versions
rust-analyzer                         # IDE: hover for inferred types; invaluable
```

Debugging network issues: `nc -v 127.0.0.1 6380`, `tcpdump -i lo0 -A port 6380` (macOS loopback is `lo0`), `redis-cli -p 6380 --verbose`, or `socat -v TCP:127.0.0.1:6380 -` to see bytes.

## 2. By task

Format: **Read** (what to read) · **Hints** (nudges without spoilers) · **rsedis** (where the same idea lives; paths checked against a clone of the repo).

### Phase A: Foundations

**Task 1: TCP echo**
- Read: Book Ch. 1–3 (setup, variables, types, control flow), Ch. 9 (`Result`, `?`), Ch. 12 for how a small I/O program is structured. [`std::net::TcpListener`](https://doc.rust-lang.org/std/net/struct.TcpListener.html).
- Hints: `listener.incoming()` yields `io::Result<TcpStream>`. `read` returns the number of bytes; slice the buffer with `&buf[..n]`. `write_all`, not `write`, because `write` may write fewer bytes than given.
- rsedis: `networking/src/lib.rs` (`handle_listener!` macro is the accept loop).

**Task 2: Threads**
- Read: Book Ch. 16.1 (threads). [`thread::spawn`](https://doc.rust-lang.org/std/thread/fn.spawn.html).
- Hints: If the closure borrows anything, `'static` bound fails; `move` transfers ownership. Use `thread::Builder::new().name(...)` to name threads for debugging.

**Task 3: Buffered reading**
- Read: Book Ch. 4.3 (slices) and Ch. 8.2 (strings). [`BufRead`](https://doc.rust-lang.org/std/io/trait.BufRead.html).
- Hints: `&TcpStream` implements `Read` and `Write`, so you can have a `BufReader<&TcpStream>` and still write to `&stream`. `read_line` appends, so clear your `String` each loop. `split_ascii_whitespace` returns an iterator over `&str`. No allocation.

**Task 4: RESP values**
- Read: Book Ch. 5 (structs), Ch. 6 (enums, `Option`), Ch. 11 (writing tests). [RESP spec](https://redis.io/docs/latest/develop/reference/protocol-spec/) ("RESP simple strings", "bulk strings", "arrays").
- Hints: Write the test for each variant *first*, then `encode`. `write!(out, ...)` works on `Vec<u8>` if `std::io::Write` is in scope (it implements `Write`). Integer formatting: `itoa`-free, use `write!(out, ":{}\r\n", n)`.
- rsedis: `response/src/lib.rs` (`Response` enum, `Response::as_bytes`).

### Phase B: Protocol

**Task 5: Parser**
- Read: Book Ch. 9.2–9.3 (error propagation, when to panic), Ch. 6.3 (`if let`), [`slice` methods](https://doc.rust-lang.org/std/primitive.slice.html) (`windows`, `starts_with`, `split_at`, `get`).
- Hints: Use `slice.get(i)` instead of `slice[i]` when the index might be out of range; it returns `Option`. A helper `fn read_line(buf: &[u8]) -> Result<(&[u8], usize), ParseError>` that finds `\r\n` makes every type's parser tiny. Parse in `&[u8]`; only convert to `String` for Simple/Error. For `Incomplete`, return it *as soon as* you see the buffer ends early, and don't treat it as invalid.
- rsedis: `parser/src/lib.rs` (`parse`, `ParseError::Incomplete`). It records `Argument { pos, len }` offsets into the buffer rather than copying.

**Task 6: Connection loop**
- Read: Book Ch. 5.3 (methods), Ch. 18.3 or Ch. 19 (patterns, `while let`). [`Vec::drain`](https://doc.rust-lang.org/std/vec/struct.Vec.html#method.drain).
- Hints: Read into a temporary `[u8; 4096]` then `extend_from_slice` into the connection's buffer. After parse success, `self.buf.drain(..consumed);`. If `read` returns 0 and the buffer is non-empty, the peer closed mid-frame.
- rsedis: `networking/src/lib.rs` (`Client::run`: read, parse, execute, loop) and the `Parser` struct in `parser/src/lib.rs`.

**Task 7: Command enum**
- Read: Book Ch. 6.2 (`match`), Ch. 19 (pattern syntax: guards, `@` bindings, slice patterns). [`TryFrom`](https://doc.rust-lang.org/std/convert/trait.TryFrom.html).
- Hints: Slice patterns make arity checks clean: `match args.as_slice() { [cmd] => ..., [cmd, a] => ..., _ => Err(WrongArity) }`. Uppercase the command name with `to_ascii_uppercase()` on the bytes and match on `b"PING"` byte-string literals.
- rsedis: `command/src/command.rs`.

**Task 8: Modules**
- Read: Book Ch. 7 (packages, crates, modules). Ch. 11.3 (test organization). [`cargo doc`](https://doc.rust-lang.org/cargo/commands/cargo-doc.html).
- Hints: Integration tests in `tests/` can only see your `pub` API, which is the point. Port `0` + `local_addr()` is the standard "ephemeral port" trick. If you get "private type in public interface", make the type `pub` or reduce the function's visibility.

### Phase C: Core store

**Task 9: KV store**
- Read: Book Ch. 8.3 (`HashMap`), Ch. 10.3 (lifetime elision). [`HashMap::entry`](https://doc.rust-lang.org/std/collections/hash_map/enum.Entry.html).
- Hints: `HashMap<Vec<u8>, _>::get` accepts `&[u8]` because `Vec<u8>: Borrow<[u8]>`. No allocation to look up. `remove` returns `Option<V>`; use `.is_some()` for DEL counts.
- rsedis: `database/src/lib.rs` (`Database` struct and `get`/`get_or_create`).

**Task 10: Shared state**
- Read: Book Ch. 16.3 (`Mutex` and `Arc`), Ch. 16.4 (`Send`/`Sync`). [`MutexGuard`](https://doc.rust-lang.org/std/sync/struct.MutexGuard.html).
- Hints: Scope the guard with a block `let reply = { let mut db = shared.lock().unwrap(); execute(&mut db, cmd) };` to unlock before I/O. `Arc::clone(&x)` is the idiom that signals "cheap". `std::sync::Mutex` is fine here; `parking_lot` is optional later.
- rsedis: `networking/src/lib.rs` shares `Arc<Mutex<Database>>` and scopes the lock to one `command::command(...)` call.

**Task 11: Errors**
- Read: Book Ch. 9 in full. [Error Handling in Rust](https://doc.rust-lang.org/std/error/trait.Error.html) trait docs. API guidelines on error types.
- Hints: Implement `From<io::Error> for Error` so `?` converts automatically. Two layers help: an internal `Error` for fatal problems and a `Value::Error` produced for client-visible ones. Keep "reply with error" and "close connection" as separate control-flow decisions.

**Task 12: Options & integers**
- Read: Book Ch. 13.1–13.2 (closures, iterators), [`Option`](https://doc.rust-lang.org/std/option/enum.Option.html) and [`Result`](https://doc.rust-lang.org/std/result/enum.Result.html) method lists. [`i64::checked_add`](https://doc.rust-lang.org/std/primitive.i64.html#method.checked_add).
- Hints: To parse bytes as an integer: `std::str::from_utf8(b).ok().and_then(|s| s.parse::<i64>().ok())`. Redis is strict about what counts as an integer (leading spaces, `+1`, `01`); check each against real Redis (`set a " 1"` then `incr a`) and match it. Loop over options with `let mut it = args.iter().peekable();`.

**Task 13: Expiration**
- Read: [`Instant`](https://doc.rust-lang.org/std/time/struct.Instant.html), [`Duration`](https://doc.rust-lang.org/std/time/struct.Duration.html). Redis [EXPIRE docs](https://redis.io/docs/latest/commands/expire/) and [key expiry internals](https://redis.io/docs/latest/commands/expire/#how-redis-expires-keys).
- Hints: After changing the value type, let the compiler guide you with `cargo check` and fix errors top to bottom. Sampling random keys from a `HashMap`: keep a parallel `Vec` of expiring keys, or scan from a rotating cursor. A true random sample is not required. For the background thread, hold a `Weak<Mutex<Db>>` or an `Arc<AtomicBool>` stop flag so the thread can end.
- rsedis: `database/src/lib.rs` (`data_expiration_ms`, `is_expired`, `active_expire_cycle`).

### Phase D: Data types

**Task 14: Lists**
- Read: Book Ch. 6 (matching enums), [`VecDeque`](https://doc.rust-lang.org/std/collections/struct.VecDeque.html), Redis [LRANGE](https://redis.io/docs/latest/commands/lrange/) (index rules).
- Hints: Normalize a Redis index with `let i = if i < 0 { len + i } else { i }` using `i64`, clamp, then convert via `usize::try_from`. `match db.get_mut(key) { Some(Value::List(l)) => ..., Some(_) => wrongtype, None => ... }`. Use `VecDeque::range` or `iter().skip(a).take(n)`.
- rsedis: `database/src/list.rs`.

**Task 15: Sets**
- Read: Book Ch. 13.2 (iterators), [`Iterator`](https://doc.rust-lang.org/std/iter/trait.Iterator.html) method list, [`HashSet`](https://doc.rust-lang.org/std/collections/struct.HashSet.html).
- Hints: `a.intersection(&b)` yields `&Vec<u8>`; `.cloned().collect()`. For N-way intersection, fold: start with a clone of the smallest set and `retain(|m| other.contains(m))` for each other set. When the iterator type is awkward to name, return `Vec`, and optimize later.
- rsedis: `database/src/set.rs`.

**Task 16: Hashes & traits**
- Read: Book Ch. 10.2 (traits), Ch. 17 or 18 (trait objects), [`dyn` compatibility](https://doc.rust-lang.org/reference/items/traits.html#dyn-compatibility), API guidelines on traits.
- Hints: Start by writing the three "get-or-wrongtype" helpers by hand; the trait emerges from what's identical. A generic helper `fn with_list<R>(&mut self, key: &[u8], f: impl FnOnce(&mut VecDeque<..>) -> R) -> Result<Option<R>, WrongType>` removes much of the repetition. Closures as parameters are a core Rust skill.

**Task 17: Sorted sets & crates**
- Read: [`Ord`](https://doc.rust-lang.org/std/cmp/trait.Ord.html), [`f64::total_cmp`](https://doc.rust-lang.org/std/primitive.f64.html#method.total_cmp), [`BTreeSet`](https://doc.rust-lang.org/std/collections/struct.BTreeSet.html), [Cargo: specifying dependencies](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html), [`thiserror`](https://docs.rs/thiserror).
- Hints: A `struct Score(f64)` with `impl Ord { fn cmp(&self, o:&Self)->Ordering { self.0.total_cmp(&o.0) } }` plus `Eq`/`PartialOrd` (delegating to `cmp`) is enough, provided you reject NaN at the boundary. Update both indexes inside one method and keep fields private so the invariant can't be broken from outside. For score formatting, note that Rust's `{}` on `1.0_f64` prints `1`, which happens to match Redis; check `inf` and very large or small values against real Redis.
- rsedis: `database/src/zset.rs` (uses the `skiplist` crate plus a `HashMap`).

### Phase E: Operability

**Task 18: Config & glob**
- Read: [`clap` derive tutorial](https://docs.rs/clap/latest/clap/_derive/_tutorial/index.html), Book Ch. 12 (CLI app), [`FromStr`](https://doc.rust-lang.org/std/str/trait.FromStr.html).
- Hints: Build the glob matcher as a pure function `fn glob(pat: &[u8], s: &[u8]) -> bool` with recursion on `*`. Test it exhaustively before wiring it in. Keep per-connection state in a `struct Session { db: usize, ... }` and pass it into `execute`.
- rsedis: `config/src/lib.rs`, `util/src/lib.rs` (`glob_match`).

**Task 19: Shutdown & logging**
- Read: Book Ch. 16.2 (message passing), Ch. 15.3 (`Drop`), [`AtomicBool`](https://doc.rust-lang.org/std/sync/atomic/struct.AtomicBool.html), [`log`](https://docs.rs/log), [`env_logger`](https://docs.rs/env_logger), [`ctrlc`](https://docs.rs/ctrlc).
- Hints: `Ordering::Relaxed` is fine for a lone stop flag; `SeqCst` is a safe default if unsure. Blocking `accept()` won't see the flag: set the listener non-blocking and poll, or connect to yourself to wake it. Same for per-connection reads: `set_read_timeout(Some(Duration::from_millis(200)))`.
- rsedis: `logger/src/lib.rs`.

**Task 20: Pub/Sub**
- Read: Book Ch. 16.2, [`mpsc`](https://doc.rust-lang.org/std/sync/mpsc/index.html), Redis [pub/sub docs](https://redis.io/docs/latest/develop/interact/pubsub/).
- Hints: `stream.try_clone()` gives a second handle for the writer thread. `Sender::send` returns `Err` when the receiver is gone; use it to prune the registry. Message format: `["message", channel, payload]` array. Subscribe confirmation: `["subscribe", channel, count]`.
- rsedis: `database/src/lib.rs` (`subscribe`/`publish`), `networking/src/lib.rs` (`create_writer_thread`).

**Task 21: Transactions**
- Read: Redis [transactions docs](https://redis.io/docs/latest/develop/interact/transactions/), [`std::mem::take`](https://doc.rust-lang.org/std/mem/fn.take.html).
- Hints: Make the session state an enum and `match` on it at the top of command handling. Execute the queue by locking once and looping `execute_locked(&mut db, cmd)`. This only works if you separated "lock" from "execute" in Task 10.
- rsedis: `command/src/command.rs` (`multi`, `exec`, `watch`), `database/src/lib.rs` (`key_watch`, `key_updated`).

### Phase F: Async

**Task 22: tokio port**
- Read: Book async chapter (Ch. 17 in recent editions), [Tokio tutorial](https://tokio.rs/tokio/tutorial) (Hello Tokio → Framing → Shared state), [`bytes`](https://docs.rs/bytes), [Asynchronous Programming in Rust](https://rust-lang.github.io/async-book/) (for the model; skim).
- Hints: `Cargo.toml`: `tokio = { version = "1", features = ["full"] }`. `stream.read_buf(&mut bytesmut)` appends to a `BytesMut`; your parser takes `&bytesmut[..]`; call `bytesmut.advance(n)` after success. "future cannot be sent between threads safely: MutexGuard held across await": scope the lock in a block. Don't use `tokio::sync::Mutex` for the `Db`, since the operations are short and synchronous.

**Task 23: Async pub/sub, blocking ops**
- Read: Tokio tutorial "Channels", "Select", [`tokio::sync`](https://docs.rs/tokio/latest/tokio/sync/index.html), Tokio [graceful shutdown](https://tokio.rs/tokio/topics/shutdown), [cancellation safety](https://docs.rs/tokio/latest/tokio/macro.select.html#cancellation-safety).
- Hints: `Notify::notified()` has a subtle race (a notification before you await it). Create the `Notified` future, *then* check the list, *then* await. `select!` drops the losing branches; make sure that doesn't drop popped data. Pop only after wake-up, in the same lock scope as the check.
- rsedis: no async version. Its blocking `BLPOP` (`generic_bpop` in `command/src/command.rs`) is worth reading to see what the thread-based alternative costs.

### Phase G: Durability & extras

**Task 24: Snapshots**
- Read: [`std::fs`](https://doc.rust-lang.org/std/fs/index.html), [`std::io::Write`](https://doc.rust-lang.org/std/io/trait.Write.html), [`u32::to_le_bytes`](https://doc.rust-lang.org/std/primitive.u32.html#method.to_le_bytes), [`tempfile`](https://docs.rs/tempfile) (or write to `dump.rdb.tmp` and `fs::rename`), [`crc32fast`](https://docs.rs/crc32fast) (optional).
- Hints: Make the encoder generic over `W: Write` and the decoder over `R: Read`; then test with `Vec<u8>` / `Cursor<&[u8]>` instead of files. Length-prefix everything. `read_exact` returns `UnexpectedEof` on truncation, which you should convert to a clear error.

**Task 25: AOF**
- Read: Redis [persistence docs](https://redis.io/docs/latest/operate/oss_and_stack/management/persistence/), [`File::sync_data`](https://doc.rust-lang.org/std/fs/struct.File.html#method.sync_data).
- Hints: Reuse `Value::encode` for the log record and `parse` for replay; you already wrote both. Log *after* the command succeeds and *before* replying. Replay must not log again. Detect a torn tail by `ParseError::Incomplete` at EOF, and `set_len` to truncate.

**Task 26: Performance**
- Read: [The Rust Performance Book](https://nnethercote.github.io/perf-book/), [`criterion`](https://docs.rs/criterion), [`cargo-flamegraph`](https://github.com/flamegraph-rs/flamegraph) (needs `sudo`/`dtrace` on macOS) or [`samply`](https://github.com/mstange/samply), [`redis-benchmark`](https://redis.io/docs/latest/operate/oss_and_stack/management/optimization/benchmarks/).
- Hints: Always `--release`. Add `[profile.release] debug = true` for readable flamegraphs. Don't guess: the top of the flamegraph is usually allocation (`Vec` clones in replies) or lock contention. Sharding: `hash(key) % N`, and for multi-key ops lock shards in ascending index order.

**Task 27: Stretch goals**
- Read: Redis [replication docs](https://redis.io/docs/latest/operate/oss_and_stack/management/replication/), [RESP3 spec](https://github.com/redis/redis-specifications/blob/master/protocol/RESP3.md), Rustonomicon, [`cargo miri`](https://github.com/rust-lang/miri).
- rsedis: nothing to compare. `SLAVEOF` and `PSYNC` appear in its command table only.

## 3. Navigating rsedis

rsedis is a Cargo workspace of small crates. Most of them are a single `src/lib.rs`, and two files hold most of the code: `database/src/lib.rs` (about 4,400 lines including tests) and `command/src/command.rs` (about 6,000). Paths below were checked against a clone of the repo.

| rsedis crate | Role | Your equivalent |
|---|---|---|
| `parser` | Request parsing into `Argument { pos, len }` offsets; the streaming `Parser` | `resp::parse` (Tasks 5–6) |
| `response` | `Response` enum and `as_bytes()` | `resp::Value::encode` (Task 4) |
| `command` (`command.rs`, `aof.rs`) | Command table and one function per command; AOF loader | `Command` enum + `execute` (Task 7+), Task 25 |
| `database` (`lib.rs`, `list.rs`, `set.rs`, `string.rs`, `zset.rs`, `dbutil.rs`, `error.rs`) | Keyspace, value types, expiry, pub/sub, key watching | `db` module (Tasks 9–17, 20, 21) |
| `database/rdbutil` | Redis RDB value encoding (used by `DUMP`) | Task 24 (optional inspiration) |
| `networking` (`lib.rs`) | Listener threads, `Client::run`, writer thread | `server.rs` (Tasks 2, 10, 19, 20) |
| `persistence` (`aof.rs`) | AOF writer | Task 25 |
| `config` | Config file and CLI parsing | Task 18 |
| `logger` | Log levels and output | Task 19 |
| `util` | `glob_match`, helpers | Task 18 |
| `compat` | Platform shims (unix/win) | not needed |

Differences to keep in mind when reading it:
- It was started in 2015 and later updated to the 2018 edition. You will still see `extern crate`, macro-heavy code (`try_validate!`, `sendlog!`), and the deprecated `Error::description`/`cause` methods. Don't copy its syntax; its README asks for nightly Rust.
- It is thread-based with no `async`: a thread per client plus a writer thread per client, and a thread per listener.
- `SAVE`/`BGSAVE` and replication (`SLAVEOF`, `PSYNC`) are not implemented. It does implement an AOF, `MULTI`/`EXEC`/`WATCH`, pub/sub, blocking list pops and a fairly wide command set.
- Commands live in one very large `match`; Task 7 steers you towards a typed `Command` enum instead.
- Its blocking commands and a few other places are acknowledged as awkward by the author. Read them as "what the alternative costs".

For working implementations of the hard parts, see [SOLUTIONS.md](SOLUTIONS.md).

## 4. Redis behavior cheat sheet

Reply shapes you'll need constantly (RESP2):

| Situation | Bytes |
|---|---|
| `OK` | `+OK\r\n` |
| Error | `-ERR message\r\n` |
| Wrong type | `-WRONGTYPE Operation against a key holding the wrong kind of value\r\n` |
| Integer | `:1000\r\n` |
| Bulk string | `$5\r\nhello\r\n` |
| Nil bulk | `$-1\r\n` |
| Empty array | `*0\r\n` |
| Nil array (e.g. timed-out `BLPOP`) | `*-1\r\n` |
| Unknown command | `-ERR unknown command 'foo', with args beginning with: \r\n` |
| Wrong arity | `-ERR wrong number of arguments for 'get' command\r\n` |
| Not an integer | `-ERR value is not an integer or out of range\r\n` |
| Syntax | `-ERR syntax error\r\n` |

Requests are always arrays of bulk strings. The inline command format (plain `PING\r\n` from `nc`) is also accepted by real Redis; supporting it is an easy bonus after Task 6.

## 5. Asking for help well

If you're still stuck, ask (a colleague, the Rust forum at <https://users.rust-lang.org>, or an AI assistant) with:
1. The smallest code that reproduces the problem (try the [Rust Playground](https://play.rust-lang.org)).
2. The **full** compiler error (`cargo check 2>&1`), not a paraphrase.
3. What you expected the borrow/ownership/lifetime story to be. Writing this out often solves the problem.
