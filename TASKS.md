# rrredis: Build Redis in Rust, Step by Step

A hands-on path for a senior engineer who is new to Rust. You will build a working Redis-compatible server, and each task introduces one or two Rust concepts in the order they are needed.

- **Companion files:** [REFERENCES.md](REFERENCES.md) is for when you are stuck. [SOLUTIONS.md](SOLUTIONS.md) has tested implementations of the tricky parts, for when hints are not enough. [TESTS.md](TESTS.md) lists the recommended tests per task.
- **Architecture reference:** [rsedis](https://github.com/seppo0010/rsedis). It is a 2015-era, thread-based codebase (no `async`, heavy use of macros, `Rust nightly` in its README), so use it to see how Redis maps onto Rust crates and modules, not as a style guide. Each task points at the relevant part.
- **Toolchain:** `rustup` stable, Rust 1.85 or newer (the project uses edition 2024). You need `redis-cli` and `nc` for manual checks (`brew install redis`).

## Redis in 60 seconds

Skim this once; each task has a one-line **Redis concept** note for the idea it uses.

- **What it is:** an in-memory data-structure server. Clients send commands over TCP; the server holds a keyspace of `key → typed value` and replies.
- **Wire protocol (RESP):** length-prefixed text/binary framing. Requests are arrays of bulk strings; replies are one of five types (simple string, error, integer, bulk string, array).
- **Data types:** strings (also counters/bitmaps), lists, sets, hashes, sorted sets. Each key has exactly one type; using the wrong command on it gives `WRONGTYPE`. Empty collections vanish.
- **Execution model:** commands run one at a time, so every command is atomic. This gives cheap counters, `MULTI`/`EXEC` transactions, and no locking in user code.
- **Expiry:** per-key TTLs, enforced lazily on access and by a periodic sweep.
- **Persistence:** RDB snapshots (compact, may lose recent writes) and AOF (command log, tunable fsync).
- **Messaging:** pub/sub (ephemeral broadcast) and blocking list ops (simple queues).
- **Scale-out:** replication from a primary to read replicas.

## Ground rules

1. **Every task ends runnable.** `cargo build` is warning-clean, `cargo test` passes, and you can demo the task's "Checkpoint" against `redis-cli`.
2. **Commit at the end of each task.** If a later task goes badly, you can return to a working state.
3. **Fight the borrow checker first, then clone.** Where a task says "avoid `.clone()`", the compiler error is the lesson. Read it fully before changing code.
4. **Run `cargo clippy` and `cargo fmt` before each commit.** Clippy is a Rust teacher.
5. **No external crates until Task 17**, except where a task says otherwise. Writing the std-only version first shows you what the crates are saving you.
6. Time estimates assume 1–3 hours, including reading. If you are over 4 hours, check REFERENCES.md and then shrink the scope. A later task will revisit it.

## Roadmap

| Phase | Tasks | Theme |
|---|---|---|
| A. Foundations | 1–4 | Cargo, ownership, `Result`, threads, bytes |
| B. Protocol | 5–8 | Enums, pattern matching, parsing, modules |
| C. Core store | 9–13 | Collections, `Arc<Mutex>`, errors, `Option`, time |
| D. Data types | 14–17 | Enums with data, iterators, closures, traits, generics |
| E. Operability | 18–21 | CLI, crates, channels, atomics |
| F. Async | 22–23 | `tokio`, futures, `select!` |
| G. Durability & extras | 24–27 | Files, serialization, transactions, perf |

Suggested layout from Task 8 onward (you will grow into it):

```
src/
  main.rs          // thin: parse args, start server
  lib.rs           // pub mod ...
  resp/            // mod.rs (Value), encode.rs, parse.rs
  command/         // mod.rs (Command enum, parse), exec.rs
  db/              // mod.rs (Db), value.rs, string.rs, list.rs, ...
  server.rs        // listener + connection handling
tests/             // integration tests (black-box, over TCP)
```

---

# Phase A: Foundations

## Task 1: TCP echo server

**Goal:** Accept one connection at a time and echo bytes back.
**Redis concept:** Redis is a TCP server (default port 6379) speaking a simple request/response protocol; clients keep long-lived connections.
**Rust concepts:** `cargo run`, `fn main() -> std::io::Result<()>`, `Result` and `?`, `let`/`mut`, `std::net::{TcpListener, TcpStream}`, `Read`/`Write` traits, `&mut` references, byte buffers (`[u8; N]`), `match`, `loop`.
**Build:**
- Bind `127.0.0.1:6380` (not 6379, so you can run a real Redis alongside).
- For each incoming stream, read into a `[u8; 1024]` buffer in a loop and write back exactly the bytes read. Stop when `read` returns `0`.
- Put the per-connection logic in `fn handle(stream: TcpStream) -> io::Result<()>`.

**Checkpoint:** `nc 127.0.0.1 6380`, type lines, see them echoed. Ctrl-D ends the session and the server accepts the next client.
**Done when:** No `unwrap()` in `handle`, and you can explain why `read` needs `&mut` access to the stream (and what `impl Read for &TcpStream` changes).
**Reference:** rsedis `networking/src/lib.rs` (the `handle_listener!` macro is its accept loop; `Client::run` is the per-connection loop).

## Task 2: Thread per connection

**Goal:** Serve many clients concurrently.
**Redis concept:** Real Redis runs commands on one thread (an event loop); you start with thread-per-connection and revisit this tradeoff in Tasks 10 and 22.
**Rust concepts:** `std::thread::spawn`, `move` closures, ownership transfer, `JoinHandle`, `Send`, why `thread::spawn` requires `'static`.
**Build:**
- Spawn a thread per accepted connection. Log the peer address (`stream.peer_addr()`) on connect and disconnect with `eprintln!`.
- Handle errors inside the thread: an error in one client must not kill the server. Print it and continue.
- Deliberately try capturing a local `String` by reference in the closure. Read the error, then fix it with `move`.

**Checkpoint:** Two `nc` sessions at once, both echoing.
**Done when:** Killing a client mid-write does not panic the server.

## Task 3: Buffered reading and a line protocol

**Goal:** Stop treating the socket as raw chunks. Read *frames* from it.
**Redis concept:** Redis also accepts *inline commands* (plain text lines) alongside the real protocol; keys and values are binary-safe byte strings, not text.
**Rust concepts:** `BufReader`, `BufRead::read_line`, `String` vs `&str` vs `Vec<u8>`/`&[u8]`, UTF-8 validation (`String::from_utf8`), `trim`, `split_whitespace`, `Option`, borrowing slices of a string.
**Build:** Turn the echo server into a tiny line protocol: `PING` replies `+PONG\r\n`, `ECHO <text>` replies with the text, anything else replies `-ERR unknown command\r\n`. Case-insensitive command names.
- Use `BufReader::new(&stream)` for reading and write to `&stream`. Notice why you need both.
- Do not allocate a new `String` per token. Work with `&str` slices of the line.

**Checkpoint:** `nc` session with `PING`, `echo hi`, `FOO`.
**Done when:** You can state in one sentence the difference between `String`, `&str`, `Vec<u8>`, `&[u8]`. Redis values are binary-safe, so you will be using the byte types.

## Task 4: Modeling RESP values

**Goal:** Define the data type for the Redis wire protocol (RESP2) and serialize it.
**Redis concept:** RESP2 has 5 types: simple string `+`, error `-`, integer `:`, bulk string `$` (length-prefixed, binary-safe, `-1` = nil), array `*`. Every reply is one of these.
**Rust concepts:** `enum` with data, `struct`, `impl` blocks, `#[derive(Debug, Clone, PartialEq)]`, `Vec<u8>`, `Option<T>` for null, recursion with `Vec<Value>` (arrays of arrays), writing into `&mut Vec<u8>` or `impl Write`, unit tests with `#[cfg(test)]`.
**Build:** In `src/resp.rs` (new module; add `mod resp;` in `main.rs`):
```rust
pub enum Value {
    Simple(String),          // +OK\r\n
    Error(String),           // -ERR ...\r\n
    Integer(i64),            // :42\r\n
    Bulk(Option<Vec<u8>>),   // $3\r\nfoo\r\n  or  $-1\r\n (null)
    Array(Option<Vec<Value>>), // *2\r\n...    or  *-1\r\n (null)
}
impl Value { pub fn encode(&self, out: &mut Vec<u8>) { ... } }
```
- Read the [RESP spec](https://redis.io/docs/latest/develop/reference/protocol-spec/) first.
- Wire it into the Task 3 server: `PING` replies `Value::Simple("PONG".into()).encode(...)`.
- Optional: shorthand constructors such as `Value::ok()` and `Value::bulk(impl Into<Vec<u8>>)`. This is your first use of `Into`.

**Checkpoint:** Server still works with `nc`. Unit tests cover every variant, including nested arrays and nulls.
**Done when:** `encode` does not allocate intermediate `String`s for bulk data.
**Reference:** rsedis `response/src/lib.rs`.

---

# Phase B: Protocol

## Task 5: RESP parser (complete frames only)

**Goal:** Parse a complete RESP frame from a byte slice.
**Redis concept:** Length-prefixed framing: a parser must tell "need more bytes" apart from "malformed" because TCP delivers arbitrary fragments.
**Rust concepts:** `Result<T, E>` with a custom error enum, `?` operator, slicing (`&buf[i..j]`), `std::str::from_utf8`, `str::parse::<i64>()`, `impl std::fmt::Display` and `impl std::error::Error`, `match` on slices and tuples, recursion.
**Build:** `fn parse(buf: &[u8]) -> Result<(Value, usize), ParseError>` returning the value and bytes consumed.
- Support all five types and null bulk/array.
- `ParseError` variants: `Incomplete`, `Invalid(String)`. (Use `Incomplete` when the buffer ends mid-frame; the next task needs it.)
- Reject negative lengths other than `-1`, non-numeric lengths, missing `\r\n`.
- Handle bulk strings containing `\r\n` and arbitrary bytes. Length-prefixed means you never scan the payload.

**Checkpoint:** Round-trip test: `parse(encode(v)) == v` for a set of values. No server change needed yet.
**Done when:** Fuzz-ish manual check: feed random truncations of valid frames and confirm `Incomplete`, never a panic or out-of-bounds slice.
**Reference:** rsedis `parser/src/lib.rs` (`parse`, `ParseError::Incomplete`). It returns offsets into the buffer instead of building a value tree; compare the tradeoff.

## Task 6: Connection read loop with a growing buffer

**Goal:** Accept pipelined and fragmented requests correctly.
**Redis concept:** *Pipelining*: clients may send many commands without waiting for replies, so one read can contain several requests or half of one.
**Rust concepts:** owning a `Vec<u8>` buffer in a struct (`struct Connection { stream, buf }`), `impl` methods with `&mut self`, `Vec::drain`/`copy_within`/`truncate`, loops with early `return`/`break`, `while let`, `io::ErrorKind`.
**Build:** `Connection::read_value(&mut self) -> io::Result<Option<Value>>`.
- Loop: try `parse(&self.buf)`. On success, remove consumed bytes and return. On `Incomplete`, `read` more from the socket. On EOF with an empty buffer return `Ok(None)`. On EOF with a partial frame return an error.
- Server loop: read a request (Array of Bulk), and for now reply `+PONG` to everything.
- Write replies with a single `write_all` per reply, and flush on batch end if you buffer writes.

**Checkpoint:**
```sh
printf '*1\r\n$4\r\nPING\r\n*1\r\n$4\r\nPING\r\n' | nc 127.0.0.1 6380   # two PONGs (pipelining)
redis-cli -p 6380 ping                                                 # PONG
```
Also send a frame one byte at a time (script it with `sleep`) and see one reply.
**Done when:** `redis-cli` and pipelined `nc` input both work.

## Task 7: Commands as an enum

**Goal:** Turn raw `Value::Array` requests into typed commands.
**Redis concept:** Requests are always an array of bulk strings (`["SET","k","v"]`). Command names are case-insensitive. Errors use a prefix (`ERR`, `WRONGTYPE`) that clients match on.
**Rust concepts:** enums as sum types, `TryFrom<Value> for Command`, pattern matching with bindings and guards, `impl From<ParseError> for ...`, `Vec<Vec<u8>>` argument handling, `match` exhaustiveness, `#[non_exhaustive]` (optional), `to_ascii_uppercase`.
**Build:** `src/command.rs`:
```rust
pub enum Command {
    Ping(Option<Vec<u8>>),
    Echo(Vec<u8>),
    // more later
}
impl Command { pub fn from_args(args: Vec<Vec<u8>>) -> Result<Command, CommandError> }
```
- `CommandError` variants: `Unknown(String)`, `WrongArity(&'static str)`, `Syntax`. Each maps to a Redis error reply, e.g. `-ERR wrong number of arguments for 'echo' command`.
- Executing a command is a separate function: `fn execute(cmd: Command) -> Value`. The parse/execute split matters later for `MULTI` and the AOF.
- Support `COMMAND` (reply with an empty array) so `redis-cli` doesn't complain on connect.

**Checkpoint:** `redis-cli -p 6380 PING hello` → `"hello"`, `ECHO x` → `"x"`, `ECHO` → arity error, `FOO` → unknown command error.
**Done when:** Adding a new command only touches `Command`, its parser, and `execute`. No stringly-typed matching spread across the code.
**Reference:** rsedis `command/src/command.rs` (about 6,000 lines, dominated by one large `match`; notice the cost).

## Task 8: Library/binary split and project structure

**Goal:** Make the code testable and the module tree clean.
**Redis concept:** `redis-cli` is the reference client; it sends `COMMAND` on connect. Nothing new in Redis itself; this is about shaping your code.
**Rust concepts:** `lib.rs` vs `main.rs`, `mod`/`pub`/`pub(crate)`/`use`, module files vs directories, re-exports (`pub use`), visibility, `tests/` integration tests, doc comments `///`, `cargo doc --open`.
**Build:**
- Move everything into `src/lib.rs` + modules per the layout above. `main.rs` should be under 20 lines.
- Expose `Server::bind(addr) -> io::Result<Server>`, `server.local_addr()`, and `server.run()`. Binding to port `0` lets integration tests pick a free port.
- Add `tests/ping.rs` that starts a server on a background thread and speaks RESP over a `TcpStream`.

**Checkpoint:** `cargo test` runs unit and integration tests. `cargo doc --open` shows your public API.
**Done when:** Nothing in `lib.rs` is `pub` that doesn't need to be.

---

# Phase C: Core store

## Task 9: In-memory key/value store (single-threaded)

**Goal:** `SET`, `GET`, `DEL`, `EXISTS`.
**Redis concept:** The keyspace is a big hash table of key → value. `SET`/`GET` are O(1); `DEL`/`EXISTS` take multiple keys and return counts.
**Rust concepts:** `HashMap<Vec<u8>, Vec<u8>>`, `&self` vs `&mut self`, returning `Option<&V>` (borrowing out of a struct), `Option<&[u8]>` vs `Option<Vec<u8>>`, `entry` API, `iter().filter().count()`, lifetime elision rules (why `get(&self, k) -> Option<&V>` just works).
**Build:** `struct Db { map: HashMap<Vec<u8>, Vec<u8>> }`.
- `execute(&mut Db, Command) -> Value`.
- For now, one `Db` owned by the accept loop and handled one connection at a time (accept the limitation on purpose; Task 10 fixes it).
- `DEL k1 k2 ...` and `EXISTS k1 k2 ...` return counts.

**Checkpoint:** `redis-cli -p 6380 set a 1`, `get a`, `del a`, `get a` → `(nil)`.
**Done when:** `Db::get` returns a borrow and you can explain why the compiler refuses to let you hold it across a `set`.
**Reference:** rsedis `database/src/lib.rs`.

## Task 10: Shared state across threads

**Goal:** Concurrent clients share one database.
**Redis concept:** Every command is *atomic* because Redis executes one at a time. With threads you recreate that guarantee with a lock around each command.
**Rust concepts:** `Arc<T>`, `Mutex<T>`, `MutexGuard` and RAII unlock, `Send`/`Sync` marker traits, lock scope (drop the guard before writing to the socket), poisoning, `Arc::clone` (cheap) vs `Clone` on data (expensive).
**Build:** `type SharedDb = Arc<Mutex<Db>>`. Each connection thread gets a clone of the `Arc`.
- Keep the critical section minimal: lock, execute, build the reply `Value`, **unlock**, then write.
- Decide how to handle a poisoned mutex (`lock().unwrap()` vs recovering). Write down your choice in a comment.
- Experiment: hold the lock during the socket write, and observe a slow client stalling everyone. Then fix it.

**Checkpoint:** Two `redis-cli` sessions: `set` in one, `get` in the other.
**Done when:** An integration test with 8 threads doing 1000 `INCR`-like read-modify-write ops (use `SET`/`GET` for now) never panics. Atomicity of the compound op comes with `INCR` in Task 12.

## Task 11: Error handling that scales

**Goal:** One error type for the crate, no `unwrap` on fallible I/O or parsing paths.
**Redis concept:** Errors are replies, not disconnects: the client gets `-ERR ...` and the connection stays usable. Only protocol violations close the connection.
**Rust concepts:** custom `enum Error`, `impl From<io::Error>`/`From<ParseError>`, `impl Display`, `impl std::error::Error`, `Box<dyn Error>` tradeoffs, `Result<T> = std::result::Result<T, Error>` alias, `?` conversions, `unwrap`/`expect` policy ("only for invariants, with a message"), `anyhow` vs `thiserror` discussion (do it by hand now; Task 17 introduces `thiserror`).
**Build:**
- Introduce `crate::Error`. Distinguish **client errors** (reply with `-ERR ...` and keep the connection) from **fatal connection errors** (log and close).
- Make wrong-type and syntax errors return the exact Redis error strings, e.g. `-WRONGTYPE Operation against a key holding the wrong kind of value`.
- Search for `unwrap()`/`expect()` in the code. Each must be justified or removed.

**Checkpoint:** Malformed input (`printf 'garbage\r\n' | nc ...`) yields an error reply or a clean disconnect, never a panic.
**Done when:** `grep -rn "unwrap()" src/` shows only test code and documented invariants.

## Task 12: Options, integers, and `INCR`

**Goal:** `SET` options plus integer commands.
**Redis concept:** `SET` flags (`NX` = only if absent, `XX` = only if present, `GET`). `INCR` family treats a string as a 64-bit signed integer; atomic counters are Redis' bread and butter.
**Rust concepts:** `Option` combinators (`map`, `and_then`, `ok_or`, `unwrap_or`), `str::parse` and `ParseIntError`, `checked_add` and overflow handling, `std::str::from_utf8`, builder-ish option parsing with a `while let Some(arg) = iter.next()` loop, `Peekable`, `matches!`.
**Build:**
- `SET key value [NX|XX] [GET]`, `SETNX`, `GETSET` (or `SET ... GET`).
- `INCR`, `DECR`, `INCRBY`, `DECRBY`, `APPEND`, `STRLEN`, `MGET`, `MSET`.
- Errors: `-ERR value is not an integer or out of range`, `-ERR increment or decrement would overflow`.
- Make `INCR` atomic by doing the read-modify-write under one lock acquisition.

**Checkpoint:** `redis-cli -p 6380 incr n` ×3 → 1, 2, 3. `set n abc` then `incr n` → error. `set k v nx` twice → OK, then `(nil)`.
**Done when:** `INCR` on `i64::MAX` returns the overflow error rather than wrapping or panicking (in debug builds, wrapping would panic).

## Task 13: Expiration

**Goal:** `EXPIRE`, `PEXPIRE`, `TTL`, `PTTL`, `PERSIST`, `SET ... EX/PX`.
**Redis concept:** *TTL / expiration*: keys can expire. Redis uses lazy expiry (check on access) plus a periodic sampling sweep so unread keys still get reclaimed.
**Rust concepts:** `std::time::{Instant, Duration, SystemTime}` (and why expiry uses `Instant`), changing the stored value type to `struct Entry { value: Vec<u8>, expires_at: Option<Instant> }`, refactoring across a codebase with the compiler's help, `Option<Instant>` comparisons, background threads with a shared `Arc`, `Weak<T>` (optional), `thread::sleep`.
**Build:**
- **Lazy expiry:** every read path checks `expires_at` and deletes the entry if it's past.
- **Active expiry:** a background thread wakes every 100 ms, samples up to 20 keys with a TTL, and removes expired ones, repeating immediately if >25% were expired (Redis' real algorithm, simplified).
- `TTL` returns `-2` for a missing key, `-1` for no expiry.
- Overwriting with `SET` clears the TTL.

**Checkpoint:** `set k v ex 2`, `ttl k` → 2 or 1, wait, `get k` → `(nil)`, `dbsize` (add it if you want) → 0.
**Done when:** Expired keys don't leak memory even if never read again. Verify by looking at `DBSIZE` after waiting.
**Reference:** rsedis `database/src/lib.rs` (`data_expiration_ms`, `is_expired`, `active_expire_cycle`).

---

# Phase D: Data types

## Task 14: Typed values and lists

**Goal:** Multiple value types per key and the first collection type.
**Redis concept:** *Lists* are ordered sequences with O(1) push/pop at both ends (queues, stacks). An empty collection is deleted automatically. Using a command on the wrong type returns `WRONGTYPE`; `TYPE` reports a key's type.
**Rust concepts:** enum with data as the db value (`enum Value { Str(Vec<u8>), List(VecDeque<Vec<u8>>) }`), matching on `&mut` to mutate in place, `VecDeque`, `Entry::or_insert_with`, avoiding double lookups, `WRONGTYPE`, ranges and negative index math with `i64` ↔ `usize` conversions (`try_from`), slicing with `.iter().skip().take()`.
**Build:** `LPUSH`, `RPUSH`, `LPOP [count]`, `RPOP [count]`, `LLEN`, `LRANGE`, `LINDEX`, `LSET`, `LTRIM`.
- Empty lists must delete the key (Redis semantics). Encode this once, in one helper, not in each command.
- Rename the old string value type so there's a clear distinction between "db value" and "RESP value" in the code. This is a good moment to feel the cost of poor names.

**Checkpoint:** `rpush l a b c`, `lrange l 0 -1`, `lpop l`, `type l` (add `TYPE`), `get l` → WRONGTYPE.
**Done when:** `LRANGE` handles out-of-range and negative indices exactly like `redis-cli` against a real Redis (compare side by side; see TESTS.md for a differential script).
**Reference:** rsedis `database/src/list.rs`.

## Task 15: Sets and iterators

**Goal:** `SADD`, `SREM`, `SMEMBERS`, `SISMEMBER`, `SCARD`, `SINTER`, `SUNION`, `SDIFF`, `SPOP`.
**Redis concept:** *Sets*: unordered unique members with membership tests and set algebra (`SINTER`, `SUNION`, `SDIFF`). Used for tags, dedup, relations.
**Rust concepts:** `HashSet<Vec<u8>>`, iterator adapters (`filter`, `map`, `collect`, `fold`, `cloned`, `copied`), `impl Iterator` return types, closures capturing by reference, `HashSet::intersection` returns borrowing iterators (lifetimes in practice), collecting into `Result<Vec<_>, _>`, `into_iter` vs `iter` vs `iter_mut`.
**Build:** Add `Value::Set(HashSet<..>)`. Multi-key ops must lock once and tolerate missing keys as empty sets.
- Implement `SINTER` for N keys by starting from the smallest set.
- Avoid cloning whole sets; clone only the members you output.

**Checkpoint:** `sadd s a b c`, `sadd t b c d`, `sinter s t`, `sunion s t`, `sdiff s t`.
**Done when:** You can refactor one command from a `for` loop into an iterator chain, and back, and say which is clearer for it.
**Reference:** rsedis `database/src/set.rs`.

## Task 16: Hashes, and cutting duplication with traits

**Goal:** `HSET`, `HGET`, `HDEL`, `HGETALL`, `HEXISTS`, `HLEN`, `HINCRBY`, and a trait-based design for value types.
**Redis concept:** *Hashes*: a key holding a field → value map (like a row or object). Fields are updated individually; `HINCRBY` is an atomic per-field counter.
**Rust concepts:** `HashMap<Vec<u8>, HashMap<Vec<u8>, Vec<u8>>>` ergonomics, **traits**: define `trait TypeName { fn type_name(&self) -> &'static str; }` or a `trait Keyspace` for type-specific behavior, default methods, `impl Trait for Type`, trait objects (`Box<dyn Trait>`) vs generics vs enum dispatch and when each is right, `impl<T: Trait> ...` bounds, `where` clauses.
**Build:** Add the hash type. Then refactor the repeated pattern "get key, check type, mutate, delete if empty" into a helper (generic function or small trait) so lists, sets, and hashes share it.
- Write a short note (in a comment or `docs/design.md`) on why you chose enum dispatch or trait objects for `Value`. rsedis uses an enum; ask yourself why.

**Checkpoint:** `hset h f v`, `hgetall h`, `hincrby h n 5`.
**Done when:** Adding a new collection type needs no changes to the existing list/set code.

## Task 17: Sorted sets, `Ord`, and your first crates

**Goal:** `ZADD`, `ZREM`, `ZSCORE`, `ZRANK`, `ZRANGE [WITHSCORES]`, `ZCARD`, `ZINCRBY`.
**Redis concept:** *Sorted sets*: unique members ordered by a float score, ties by member bytes. Used for leaderboards and range queries; ranks are 0-based. Real Redis uses a skip list.
**Rust concepts:** `BTreeMap`/`BTreeSet`, implementing `Ord`/`PartialOrd`/`Eq` for a newtype around `f64` (floats aren't `Ord`; why), tuple ordering `(score, member)`, `Reverse`, dual-index structure (`HashMap<member, score>` + `BTreeSet<(Score, member)>`) and keeping it consistent, **adding crates**: `cargo add thiserror`, replace your hand-written `Error` boilerplate from Task 11.
**Build:** Maintain both indexes on every mutation. `ZRANGE` by rank is O(n) with `BTreeSet` iteration; that's acceptable (Redis uses a skip list; note this as a known tradeoff).
- Parse scores as `f64`, with `inf`/`-inf`, and reject NaN.
- Format scores in replies like Redis (`1`, not `1.0`; `1.5`).

**Checkpoint:** `zadd z 1 a 2 b 3 c`, `zrange z 0 -1 withscores`, `zrank z b`, `zincrby z 5 a`.
**Done when:** A test with random ops verifies both indexes always agree (property-style test; see TESTS.md).
**Reference:** rsedis `database/src/zset.rs` (a `skiplist` crate plus a `HashMap`, the same two-index idea).

---

# Phase E: Operability

## Task 18: Config, CLI args, `KEYS`, `SELECT`

**Goal:** Configurable server, multiple logical databases, key scanning.
**Redis concept:** Redis has numbered logical databases (`SELECT 0..15`, default 16), config via file/flags, `KEYS` (glob, O(N), avoid in production) and `SCAN` (incremental cursor iteration).
**Rust concepts:** `std::env::args`, then `clap` (derive macros, attribute macros), `Vec<Db>` indexed by connection state, per-connection state struct, `str::parse::<SocketAddr>`, recursive or iterative glob matching on `&[u8]`, `Cow<'_, str>` (optional), `impl FromStr`.
**Build:**
- `rrredis --port 6380 --bind 127.0.0.1 --databases 16`.
- `SELECT n`, `DBSIZE`, `FLUSHDB`, `FLUSHALL`, `KEYS pattern` (`*`, `?`, `[abc]`, `\` escapes), `SCAN cursor [MATCH p] [COUNT n]` (a simple cursor over a snapshot of keys is acceptable).
- Each connection tracks its selected database index.

**Checkpoint:** `redis-cli -p 7000 -n 3 set a 1`; `keys *`; `select 0` shows other data.
**Done when:** The glob matcher has table-driven tests and no panics on malformed patterns.
**Reference:** rsedis `config/src/lib.rs`, `util/src/lib.rs` (`glob_match`).

## Task 19: Logging and graceful shutdown

**Goal:** Clean, observable lifecycle.
**Redis concept:** `INFO` reports server stats; production servers must shut down cleanly and log. Redis also has `SHUTDOWN`.
**Rust concepts:** `AtomicBool`/`Ordering`, `Arc<AtomicBool>`, `std::sync::mpsc` channels, `JoinHandle::join`, `Drop` (RAII for cleanup), `ctrlc` crate (or `signal-hook`), `log` + `env_logger` crates facade pattern, setting a read timeout on sockets to poll the shutdown flag, `SO_REUSEADDR` considerations.
**Build:**
- Replace `eprintln!` with `log::{info, warn, error}`. Log connects, disconnects, and command errors at appropriate levels. `RUST_LOG=debug` shows commands.
- On Ctrl-C: stop accepting, let in-flight connections finish their current command, join all threads, exit 0.
- Track the number of connected clients and expose `INFO` (just a few fields: `redis_version`, `connected_clients`, `uptime_in_seconds`, `used_memory` can be fake).

**Checkpoint:** Run, connect a client, press Ctrl-C; the server logs "shutting down" and exits without panic. `redis-cli info` returns text.
**Done when:** `cargo test` has a test that starts and stops the server and asserts that the thread joined.

## Task 20: Pub/Sub with channels

**Goal:** `SUBSCRIBE`, `UNSUBSCRIBE`, `PUBLISH`, `PSUBSCRIBE` (pattern reuses Task 18's glob).
**Redis concept:** *Pub/Sub*: fire-and-forget messaging. Subscribers get messages only while connected; nothing is stored. A subscribed connection is in a restricted mode and can only manage subscriptions.
**Rust concepts:** `mpsc::channel`/`Sender`/`Receiver`, `HashMap<Vec<u8>, Vec<Sender<Message>>>` in shared state, dead-receiver cleanup (`send` returning `Err`), two sources of wake-ups in one connection thread (socket read **and** channel receive), why this is awkward in blocking code (preview of why async exists), `recv_timeout`/`try_recv`, a writer thread per subscribed connection, `Clone` on senders.
**Build:**
- A subscribed connection can only run `(P)SUBSCRIBE`, `(P)UNSUBSCRIBE`, `PING`, `QUIT`.
- `PUBLISH ch msg` returns the number of receivers.
- Solve the dual-wait problem with a reader thread plus a writer thread per subscribed client (or `try_clone()` on the stream). Document the downside.

**Checkpoint:** Terminal A: `redis-cli -p 6380 subscribe news`. Terminal B: `redis-cli -p 6380 publish news hello`; A prints the message.
**Done when:** Closing the subscriber removes it from the registry (verify via `PUBLISH` returning `0`).
**Reference:** rsedis `database/src/lib.rs` (`subscribe`, `publish`) and `networking/src/lib.rs` (`create_writer_thread`, the trick that makes pub/sub workable with blocking I/O).

## Task 21: Transactions (`MULTI`/`EXEC`/`DISCARD`)

**Goal:** Per-connection command queues executed atomically.
**Redis concept:** *Transactions*: `MULTI` queues commands, `EXEC` runs them atomically with no interleaving. No rollback: a failing command doesn't undo the others. `WATCH` adds optimistic locking.
**Rust concepts:** per-connection state machines with enums (`enum ConnState { Normal, Queuing(Vec<Command>) }`), `std::mem::take`/`replace`, holding the lock across several commands, error aggregation, `WATCH` (stretch: version counters per key).
**Build:**
- `MULTI` → `+OK`; subsequent commands reply `+QUEUED`; `EXEC` runs them under a **single lock acquisition** and returns an array of replies; `DISCARD` clears.
- Arity/syntax errors during queuing abort the transaction (`-EXECABORT`); runtime errors do not roll back the others (like Redis).
- Stretch: `WATCH`/`UNWATCH` via per-key version numbers.

**Checkpoint:** `redis-cli`: `multi`, `set a 1`, `incr a`, `exec` → `1) OK 2) (integer) 2`.
**Done when:** A test shows no other client's command can interleave between two commands in `EXEC`.
**Reference:** rsedis `command/src/command.rs` (`multi`, `exec`, `watch`) and `database/src/lib.rs` (`key_watch`, `key_updated`).

---

# Phase F: Async

## Task 22: Port the server to `tokio`

**Goal:** Same behavior, async I/O, one task per connection.
**Redis concept:** Redis is single-threaded around an event loop; async tasks multiplexed on few threads are the closest Rust analogue and avoid thread-per-client cost.
**Rust concepts:** `async fn`, `.await`, futures are lazy state machines, `#[tokio::main]`, `tokio::spawn` (and why it needs `Send + 'static`), `tokio::net::{TcpListener, TcpStream}`, `AsyncReadExt`/`AsyncWriteExt`, `bytes::BytesMut`, `std::sync::Mutex` vs `tokio::sync::Mutex` (hint: don't hold a std guard across `.await`), `tokio::select!`.
**Build:**
- Read the [tokio tutorial](https://tokio.rs/tokio/tutorial), especially "mini-redis".
- Port the accept loop and `Connection` (your parser is reusable; it only needs `&[u8]`, which is the payoff of Task 5's design).
- Keep `Db` behind `std::sync::Mutex` and never hold the guard across `.await`.
- Keep the blocking server behind a cargo feature or a git tag so you can compare.

**Checkpoint:** All earlier integration tests pass against the async server with only the test harness changed.
**Done when:** `redis-benchmark -p 6380 -t set,get -n 100000 -c 50` runs successfully and you've recorded the number from Task 10's threaded version for comparison.

## Task 23: Async pub/sub, blocking list ops, and shutdown

**Goal:** Use async to simplify what was awkward in Tasks 19–20, and add blocking commands.
**Redis concept:** *Blocking commands* (`BLPOP`) park a client until data arrives or a timeout expires; they underpin simple work queues. Pub/sub fan-out maps naturally onto broadcast channels.
**Rust concepts:** `tokio::sync::{broadcast, mpsc, oneshot, Notify, watch}`, `tokio::select!` over socket and channel, `tokio::time::timeout`/`sleep`/`interval` (replace the expiry thread), cancellation safety, `CancellationToken` or a `watch` channel for shutdown, task `JoinSet`.
**Build:**
- Rewrite pub/sub with a `broadcast` channel per channel name; a single `select!` per subscribed connection replaces the reader/writer thread pair.
- `BLPOP key [key ...] timeout` / `BRPOP`: park the task on a `Notify`, wake when a push happens, honor the timeout.
- Active expiry becomes a `tokio::spawn`ed `interval` task.
- Graceful shutdown via signal + `watch`/token.

**Checkpoint:** `redis-cli blpop q 0` blocks; from another terminal `rpush q x`; the first prints `x`. `blpop q 1` returns `(nil)` after 1s.
**Done when:** There are no `thread::spawn` or `thread::sleep` calls left, and cancelling a client mid-`BLPOP` doesn't lose or duplicate list elements.
**Reference:** rsedis has no async, but its `BLPOP` (`generic_bpop` in `command/src/command.rs`, `ResponseError::Wait`) shows what blocking commands cost without it.

---

# Phase G: Durability & extras

## Task 24: Snapshot persistence (`SAVE`, `BGSAVE`, load on start)

**Goal:** Survive restarts.
**Redis concept:** *RDB snapshots*: a point-in-time binary dump of the dataset. `SAVE` blocks; `BGSAVE` forks (you'll snapshot instead). Fast to load but loses writes since the last snapshot.
**Rust concepts:** `std::fs::File`, `BufWriter`/`BufReader`, `Write`/`Read` traits generically (`fn dump<W: Write>(w: &mut W)`), manual binary encoding (`to_le_bytes`, `from_le_bytes`, `read_exact`), `tempfile` + atomic `rename`, versioned file headers, `serde` + `bincode` as an alternative (optional comparison), `Instant` vs `SystemTime` (persist absolute expiry in wall-clock time), cloning a snapshot for `BGSAVE` (or `Arc` copy-on-write ideas).
**Build:**
- Custom format: magic bytes `RRDB`, version, then `(type tag, key, expiry, payload)` records, then a checksum (use `crc32fast` if you want).
- `SAVE` blocks; `BGSAVE` snapshots under the lock quickly and writes in another thread/task.
- Load on startup if the file exists. Corrupt file → clear error, don't start with half data.
- Config: `--dbfilename`, `--dir`.

**Checkpoint:** `set a 1`, `save`, kill server, restart, `get a` → `"1"`. TTLs survive restarts and still expire at the right wall-clock time.
**Done when:** A round-trip test for every value type; truncated/corrupt files return errors, not panics.
**Reference:** rsedis does not implement `SAVE`/`BGSAVE` (unchecked in its `TODO.md`); `database/rdbutil` only has the value encoding behind `DUMP`. Redis' [RDB format notes](https://rdb.fnordig.de/file_format.html) are an optional inspiration.

## Task 25: Append-only file (AOF)

**Goal:** Durability via command log and replay.
**Redis concept:** *AOF (append-only file)*: log every write command and replay on startup. `appendfsync` trades durability (`always`) against speed (`everysec`, `no`). Non-deterministic commands are rewritten to deterministic ones.
**Rust concepts:** reusing `Command` as the log record (this is the payoff of the parse/execute split from Task 7), `impl Write` sink abstraction, `fsync` policy (`File::sync_data`), tradeoffs of `always`/`everysec`/`no`, replay by feeding commands through the same `execute`, log rewriting (stretch), error recovery on partial trailing record.
**Build:**
- Append each **write** command as a RESP array to `appendonly.aof`. Reads aren't logged.
- Resolve non-determinism before logging: `EXPIRE k 10` becomes `PEXPIREAT k <abs-ms>`, `SPOP` becomes `SREM`, etc.
- On startup, replay the AOF. A torn final record is cut off with a warning only if `--aof-load-truncated yes`; otherwise refuse to start (a corrupt length prefix mid-file looks just like a torn tail).
- Config: `--appendonly yes --appendfsync everysec`.

**Checkpoint:** Issue writes, `kill -9` the server, restart, data is present.
**Done when:** Tests inject a half-written trailing record and confirm recovery.
**Reference:** rsedis `persistence/src/aof.rs` (writer) and `command/src/aof.rs` (loader, torn-tail handling).

## Task 26: Performance pass

**Goal:** Measure first, then fix the real bottlenecks.
**Redis concept:** `redis-benchmark` measures throughput; `-P` sets the pipeline depth. Redis gets its speed from in-memory data, a single thread with no locks, and pipelining.
**Rust concepts:** `--release`, `criterion` benchmarks, `cargo flamegraph`/`samply`, `clippy::perf`, reducing allocations (`Bytes` instead of `Vec<u8>` clones, `SmallVec`/`Cow` where measured), lock sharding (`Vec<Mutex<HashMap>>` keyed by hash), `RwLock` tradeoffs, `#[inline]` (leave alone unless measured), `unsafe` (don't, but know where it lives).
**Build:**
- Baseline with `redis-benchmark -q -t ping,set,get,incr,lpush,lpop -n 200000 -c 50 -P 16` against your server and real Redis.
- Profile, find the top three costs, and fix them one at a time, re-measuring each time. Keep a `docs/perf.md` log: change, before, after.
- Try sharding the keyspace across N locks; confirm multi-key commands (`MSET`, `SINTER`, `EXEC`) still behave: lock ordering to avoid deadlock is the lesson.

**Checkpoint:** Numbers within an order of magnitude of real Redis on `-P 16` pipelined `SET`/`GET`.
**Done when:** `docs/perf.md` shows at least three measured improvements, and at least one idea that *didn't* help.

## Task 27: Capstone and stretch goals

**Redis concept:** *Replication*: a replica copies a primary by receiving a snapshot and then the stream of write commands. RESP3 extends the protocol with maps, sets, doubles and typed nulls.

Pick at least two. Each is a few hours.

- **Replication (basic):** `REPLICAOF host port`; the replica connects, receives a snapshot (Task 24 format), then streams write commands (Task 25 format). rsedis lists `SLAVEOF`/`PSYNC` in its command table but does not implement them, so there is nothing to compare with; design it from the Redis docs.
- **`SCAN` family done right:** stable cursors with `HSCAN`/`SSCAN`/`ZSCAN`.
- **More commands:** `SETRANGE`/`GETRANGE`, `SETBIT`/`GETBIT`/`BITCOUNT`, `HyperLogLog` (`PFADD`/`PFCOUNT`), `GEOADD` on top of your zset, `OBJECT ENCODING`.
- **RESP3:** `HELLO 3` and new types (maps, sets, doubles, nulls).
- **Compatibility sweep:** run a subset of the official Redis test suite's `unit/type/*` against your server and fix failures.
- **`unsafe` tour:** write a tiny intrusive skiplist for sorted sets using raw pointers, then wrap it in a safe API and test it under `cargo miri`. Read the [Rustonomicon](https://doc.rust-lang.org/nomicon/) first.
- **Binary hygiene:** `cargo deny`/`cargo audit`, `#![forbid(unsafe_code)]` on the main crate, CI with GitHub Actions running fmt, clippy `-D warnings`, test.

---

## Concept index

Where each Rust concept is first introduced, for when you want to jump back.

| Concept | Task |
|---|---|
| Cargo, `Result`, `?`, traits `Read`/`Write` | 1 |
| Ownership transfer, `move`, threads, `Send` | 2 |
| `String`/`&str`/`&[u8]`/`Vec<u8>`, borrowing slices | 3 |
| Enums with data, `impl`, derive, unit tests | 4 |
| Custom errors, slicing, parsing, recursion | 5 |
| Structs with `&mut self`, buffers, `while let` | 6 |
| `TryFrom`, pattern matching with guards | 7 |
| Modules, visibility, `lib.rs`, integration tests | 8 |
| `HashMap`, borrowing out of containers, lifetimes (elided) | 9 |
| `Arc`, `Mutex`, `Sync`, lock scope | 10 |
| `From` conversions, error design, `unwrap` policy | 11 |
| `Option`/`Result` combinators, checked arithmetic | 12 |
| Time, refactoring with the compiler, background threads | 13 |
| `VecDeque`, matching `&mut` enums, `TryFrom` for integers | 14 |
| Iterators, closures, `collect` into `Result` | 15 |
| Traits, generics, `dyn`, `where` clauses | 16 |
| `Ord` for custom types, `BTreeSet`, crates (`thiserror`) | 17 |
| `clap`, `FromStr`, per-connection state | 18 |
| Atomics, `mpsc`, `Drop`, `log` facade | 19 |
| Channels in shared state, limits of blocking I/O | 20 |
| State machines with enums, `mem::take` | 21 |
| `async`/`await`, futures, `tokio`, `Send + 'static` | 22 |
| `select!`, cancellation safety, async sync primitives | 23 |
| File I/O, binary encoding, generic `Write` | 24 |
| Command-as-data design, fsync tradeoffs | 25 |
| Profiling, sharding, lock ordering | 26 |
| `unsafe`, FFI-free systems work (optional) | 27 |
