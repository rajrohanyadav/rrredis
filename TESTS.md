# Recommended Tests per Task

Write these as you go. Each task lists **unit tests** (inside the module, `#[cfg(test)]`), **integration tests** (`tests/*.rs`, over TCP, from Task 8), and **manual checks** (`redis-cli`/`nc`). Tests are suggestions to catch the classic mistakes in that task. Add your own when you find a bug: write the failing test first.

## 0. Setup

### Commands
```sh
cargo test                      # all tests
cargo test resp::               # tests under a module path
cargo test --test ping          # one integration test file
cargo test -- --nocapture --test-threads=1   # when debugging output or shared ports
cargo clippy --all-targets -- -D warnings
```

### Integration test harness (from Task 8)

Create `tests/common/mod.rs`. Files in `tests/common/` aren't compiled as their own test crate, so each test file can `mod common;`.

```rust
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

pub struct TestServer { pub addr: SocketAddr }

pub fn start() -> TestServer {
    let server = rrredis::Server::bind("127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap();
    std::thread::spawn(move || { let _ = server.run(); });
    TestServer { addr }
}

pub struct Client { stream: TcpStream, buf: Vec<u8> }

impl Client {
    pub fn connect(addr: SocketAddr) -> Client {
        let stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        Client { stream, buf: Vec::new() }
    }
    /// Send a command as a RESP array of bulk strings.
    pub fn send(&mut self, args: &[&str]) {
        let mut out = format!("*{}\r\n", args.len()).into_bytes();
        for a in args { out.extend(format!("${}\r\n{}\r\n", a.len(), a).into_bytes()); }
        self.stream.write_all(&out).unwrap();
    }
    /// Send and return the raw reply bytes as a string. Fine for small replies;
    /// switch to your own parser once Task 5 exists.
    pub fn cmd(&mut self, args: &[&str]) -> String {
        self.send(args);
        self.read_reply()
    }
    pub fn read_reply(&mut self) -> String { /* use your resp::parse on self.buf, reading more as needed */ todo!() }
}
```

Tip: use your Task 5 parser inside `read_reply` so tests compare `Value`s, e.g. `assert_eq!(c.cmd(&["GET","a"]), Value::Bulk(Some(b"1".to_vec())))`. Tests that need "no panic" can use `std::panic::catch_unwind`, or check the server thread is still accepting connections afterwards.

### Differential testing against real Redis (from Task 9 onward)

The cheapest, highest-value test strategy: run the same command script against real Redis and your server and diff the replies.

```sh
redis-server --port 6379 --save "" --appendonly no &
cargo run --release -- --port 6380 &
cat script.txt | redis-cli -p 6379 > /tmp/real.out
cat script.txt | redis-cli -p 6380 > /tmp/mine.out
diff /tmp/real.out /tmp/mine.out
```

Keep scripts in `tests/scripts/*.txt` (e.g. `strings.txt`, `lists.txt`). Remove anything non-deterministic (`TTL` values, `SPOP`, `RANDOMKEY`, `SMEMBERS` ordering → use `SORT` or sort the output). Run `FLUSHALL` as the first line of each.

### Conventions
- Name tests after behavior: `lrange_negative_end_index_counts_from_tail`.
- Prefer table-driven unit tests for parsers: a `&[(&[u8], Expected)]` and a loop with a good failure message including the case.
- Use `assert_eq!` with `Value` and derive `Debug`/`PartialEq`. `#[derive]` pays for itself here.
- Tests share nothing. Each integration test starts its own server on port 0.

---

## Phase A: Foundations

### Task 1: TCP echo
- **Manual:** `nc` sends `hello\n` → receives `hello\n`. Send 1 MB (`head -c 1000000 /dev/urandom | nc ... | wc -c`) → same byte count back.
- **Manual:** Client disconnects immediately → server survives and accepts the next client.
- **Integration (optional):** connect, write 10 KB, read 10 KB back, compare.

### Task 2: Threads
- **Manual:** Open two `nc` sessions; interleave input; each gets only its own echo.
- **Integration:** Spawn 20 client threads, each sends a unique payload and asserts it gets exactly that payload back.
- **Failure injection:** a client connecting and immediately resetting (`SO_LINGER 0`, or `nc` + `kill -9`) doesn't take the server down.

### Task 3: Line protocol
- **Manual:** `PING` → `+PONG`, `ping` → `+PONG`, `ECHO hello world` → expected, empty line → no crash, `FOO` → `-ERR`.
- **Unit:** the line-splitting function handles leading/trailing whitespace, multiple spaces, `\r\n` vs `\n`, empty input.
- **Edge:** A line > 1 MB. Decide what should happen (a cap with an error is good) and test it.
- **Edge:** Invalid UTF-8 in the line: no panic.

### Task 4: RESP values and encode
Table-driven `encode` tests:

| Value | Expected bytes |
|---|---|
| `Simple("OK")` | `+OK\r\n` |
| `Error("ERR x")` | `-ERR x\r\n` |
| `Integer(0)`, `Integer(-1)`, `Integer(i64::MAX)`, `Integer(i64::MIN)` | `:0\r\n`, `:-1\r\n`, ... |
| `Bulk(Some(b"foo"))` | `$3\r\nfoo\r\n` |
| `Bulk(Some(b""))` | `$0\r\n\r\n` |
| `Bulk(None)` | `$-1\r\n` |
| `Bulk(Some(b"a\r\nb"))` | `$4\r\na\r\nb\r\n` |
| `Bulk(Some(vec![0, 255, 13, 10]))` (binary) | `$4\r\n\0\xff\r\n\r\n` |
| `Array(Some(vec![]))` | `*0\r\n` |
| `Array(None)` | `*-1\r\n` |
| `Array(Some(vec![Array(Some(vec![Integer(1)])), Bulk(None)]))` nested | `*2\r\n*1\r\n:1\r\n$-1\r\n` |

- **Manual:** `PING` still works over `nc`.

---

## Phase B: Protocol

### Task 5: Parser
- **Unit (happy path):** the inverse of every row in the Task 4 table, asserting both the `Value` and the `consumed` count.
- **Unit (trailing data):** `+OK\r\n+NEXT\r\n` → returns `Simple("OK")`, consumed = 5.
- **Unit (incomplete):** every proper prefix of every valid frame returns `Incomplete` (loop `for i in 0..frame.len()`). This is the most valuable test in the project.
- **Unit (invalid):** `?foo\r\n` (unknown type byte), `$abc\r\n`, `$-2\r\n`, `*-5\r\n`, `:12x\r\n`, `$3\r\nfoobar\r\n` (length mismatch → bytes after payload aren't `\r\n`), `:\r\n` (empty int), integer overflow `:99999999999999999999\r\n`.
- **Unit (binary safety):** bulk containing `\r\n` and NUL bytes round-trips.
- **Unit (nesting):** deeply nested arrays (e.g. depth 10_000) must not overflow the stack. Either implement a depth limit and test for the error, or make it iterative.
- **Unit (limits):** `$9999999999999\r\n` doesn't attempt to allocate that much. Cap the declared size (Redis uses 512 MB) and test the rejection.
- **Property-style (no crates):** loop 10_000 times: generate a random `Value` with a tiny hand-rolled PRNG (xorshift), `parse(encode(v)) == (v, len)`. Later swap in `proptest`.
- **Fuzz-style (no crates):** feed random bytes to `parse`; assert it never panics.

### Task 6: Connection loop
- **Unit/integration with in-memory stream:** make `Connection` generic over `R: Read + Write` and test with a fake stream that returns data in chosen chunk sizes (1 byte, 2 bytes, whole frame, two frames at once). Assert identical results for all chunkings.
- **Manual pipelining:** `printf '*1\r\n$4\r\nPING\r\n*1\r\n$4\r\nPING\r\n' | nc 127.0.0.1 6380` → `+PONG\r\n+PONG\r\n`.
- **Manual fragmentation:** a script that writes the frame byte by byte with `sleep 0.1` between.
- **Edge:** Client closes after half a frame → server logs and drops the connection, doesn't spin at 100% CPU. (Check with `top`.)
- **Edge:** 100 KB bulk value in a single request arrives across several `read`s.
- **Edge:** Buffer doesn't grow without bound across many requests (assert `buf.capacity()` stays reasonable after 10_000 small requests).

### Task 7: Commands
- **Unit (table-driven):** `from_args` for each command: valid, case variations (`ping`, `PiNg`), wrong arity (too few, too many), unknown commands.
- **Unit:** error strings exact: `wrong number of arguments for 'echo' command`.
- **Unit:** `execute(Command::Echo(x))` returns `Bulk(Some(x))`. `PING` → `Simple("PONG")`; `PING msg` → `Bulk(msg)`.
- **Edge:** a request that isn't an array of bulk strings (e.g. an array containing an integer) → protocol error, not a panic.
- **Edge:** empty array request `*0\r\n` → Redis ignores it; assert you don't crash or hang.
- **Manual:** `redis-cli -p 6380` interactive session starts without errors (needs `COMMAND` handling).

### Task 8: Library structure
- **Integration (`tests/ping.rs`):** start server on port 0, `PING` → `+PONG`; `ECHO` with binary payload; unknown command.
- **Integration:** two servers on different ephemeral ports in the same test process don't interfere.
- **Compile-time:** `cargo doc --no-deps` produces no warnings (add `#![warn(missing_docs)]` for the public API).
- **Hygiene:** `cargo test --doc` passes for any doc examples you add.

---

## Phase C: Core store

### Task 9: KV store
- **Unit (`Db`):** set/get/overwrite/del/exists; missing key → `None`; `del` returns whether the key existed; empty key `""` and empty value are valid; binary keys with NULs.
- **Integration:** `SET a 1` → OK; `GET a` → `"1"`; `GET missing` → `$-1`; `DEL a b c` where only `a` exists → `:1`; `EXISTS a a` (same key twice) → `:2` (Redis counts duplicates).
- **Integration:** `SET` with the wrong arity → error and the connection stays usable for the next command.
- **Differential:** `strings.txt` script vs real Redis.

### Task 10: Shared state
- **Integration:** client A sets, client B gets the same value.
- **Stress:** 16 threads × 1000 iterations of `SET key{thread}-{i}` + `GET`; afterwards `DBSIZE` equals expected total (add `DBSIZE` if needed). Run with `cargo test --release` also, since timing bugs show up faster.
- **Hang check:** a client that sends half a command and stalls doesn't block other clients. (Open a connection, send `*3\r\n$3\r\nSET`, then run a command on another connection with a read timeout of 1s.)
- **Hang check:** a client that doesn't read replies (fills the socket buffer) doesn't block others. This is the lock-during-write bug from the task.
- **Optional:** run under `cargo test -- --test-threads=1` and `RUSTFLAGS="-Z sanitizer=thread"` on nightly (`cargo +nightly test -Zbuild-std --target aarch64-apple-darwin`) to see TSan stay quiet.

### Task 11: Error handling
- **Unit:** each `Error` variant `Display`s a message, converts via `From` (`io::Error` → `Error`), and `source()` is wired up if applicable.
- **Integration:** after a command error, the same connection still works. After a protocol error (garbage bytes), the server replies `-ERR Protocol error: ...` and closes the connection, and other clients are unaffected.
- **Static:** a CI-ish check script: `! grep -rn "unwrap()" src/ | grep -v "#\[cfg(test)\]"` (approximate; review the matches by hand).
- **Fault injection:** a `Write` that returns `ErrorKind::BrokenPipe` makes the handler return `Err` without panicking.

### Task 12: Options and integers
- **Unit:** option parser: `SET k v NX`, `nx` lowercase, `NX XX` together → syntax error, `EX` without value, `EX 0` → `-ERR invalid expire time in 'set' command`, `EX abc`, `EX -1`, duplicate options.
- **Integer table (compare to real Redis):**

| Stored | Command | Expected |
|---|---|---|
| (missing) | `INCR` | `:1` |
| `"10"` | `INCRBY 5` | `:15` |
| `"10"` | `DECRBY 20` | `:-10` |
| `"abc"` | `INCR` | not an integer error |
| `"1.5"` | `INCR` | not an integer error |
| `" 1"`, `"+1"`, `"01"` | `INCR` | check real Redis, then match |
| `"9223372036854775807"` | `INCR` | overflow error |
| `"-9223372036854775808"` | `DECR` | overflow error |
| `"1"` | `INCRBY 9223372036854775807` | overflow error |
| `"1"` | `INCRBY notanumber` | not an integer error |

- **Unit:** `APPEND` on missing key creates it and returns the length; `STRLEN` of missing → `:0`; `MSET` with odd number of args → arity error; `MGET` returns nil for missing and for non-string types (later).
- **Concurrency:** 8 threads × 1000 `INCR` on the same key → final value 8000 exactly. This is the first real atomicity test.

### Task 13: Expiration
Use short TTLs (100–300 ms) and generous margins; avoid flaky timing. For determinism, abstract the clock: `trait Clock { fn now(&self) -> Instant }` with a `FakeClock` for unit tests. That is a good small exercise in traits and generics.

- **Unit (fake clock):** key readable before expiry, gone after; `TTL` before → remaining seconds (rounded as Redis does), after → `-2`; `PERSIST` removes TTL → `TTL` returns `-1`; `SET` on an existing key clears TTL; `EXPIRE` on a missing key → `:0`; `EXPIRE k 0` or negative deletes immediately; `PEXPIRE` ms precision.
- **Unit:** `SET k v EX 10 PX 100` → syntax error (mutually exclusive). `SET k v KEEPTTL` (stretch).
- **Integration (real time):** `SET k v PX 150`; `GET` immediately → value; sleep 300 ms; `GET` → nil.
- **Active expiry:** set 1000 keys with `PX 100`; never read them; sleep 1 s; `DBSIZE` → 0. This proves the background sweeper works rather than just lazy expiry.
- **Shutdown:** dropping the server stops the sweeper thread (no thread leak across tests).
- **Differential:** don't diff `TTL` raw output. Assert it's within ±1 of the expected value.

---

## Phase D: Data types

### Task 14: Lists
- **Unit (index normalization), table-driven, for list `[a,b,c,d,e]`:**

| `LRANGE start end` | Expected |
|---|---|
| `0 -1` | all |
| `0 0` | `[a]` |
| `-2 -1` | `[d,e]` |
| `1 100` | `[b..e]` |
| `-100 2` | `[a,b,c]` |
| `3 1` | `[]` |
| `10 20` | `[]` |
| `-1 -100` | `[]` |

- **Unit:** `LPUSH l a b c` yields `c b a` (each pushed to the head in order); `RPUSH l a b c` yields `a b c`. `LPUSH` returns the new length.
- **Unit:** `LPOP` on empty/missing → nil; `LPOP l 0`; `LPOP l 2` returns array, `LPOP l 100` clamps; popping the last element removes the key (`EXISTS l` → 0, `TYPE l` → `none`).
- **Unit:** `LINDEX` negative and out-of-range → nil. `LSET` out of range → `-ERR index out of range`; `LSET` on missing → `-ERR no such key`. `LTRIM` to an empty range deletes the key.
- **Integration:** `SET s x` then `LPUSH s a` → `WRONGTYPE`. `RPUSH l a` then `GET l` → `WRONGTYPE`. `TYPE` returns `string`/`list`/`none`.
- **Differential:** `lists.txt`, including all edge cases above. This is the best test for this task.

### Task 15: Sets
- **Unit:** `SADD` returns number of *new* members (`SADD s a a b` → 2); `SREM` returns the removed count; `SISMEMBER` 0/1; `SCARD`; `SMEMBERS` (compare as sets, not ordered lists).
- **Unit (set algebra), with `s={a,b,c}`, `t={b,c,d}`, `u={}`:** `SINTER s t` = `{b,c}`; `SUNION s t` = `{a,b,c,d}`; `SDIFF s t` = `{a}`; `SDIFF t s` = `{d}`; `SINTER s missing` = `{}`; `SUNION s missing` = `s`; `SDIFF missing s` = `{}`; `SINTER s` (single key) = `s`; `SINTER s s` = `s`.
- **Unit:** `SINTER` with a non-set key → `WRONGTYPE`, even if another key is missing.
- **Unit:** removing the last member deletes the key.
- **Unit:** `SPOP` returns a member that was present and is no longer. Check `SCARD` decrements. `SPOP missing` → nil.
- **Property-style:** random sets (via xorshift) → `|A ∪ B| = |A| + |B| − |A ∩ B|`.
- **Differential:** sort the output (`redis-cli ... | sort`) before comparing.

### Task 16: Hashes and traits
- **Unit:** `HSET h f1 v1 f2 v2` returns the number of *new* fields; `HGET`/`HDEL`/`HEXISTS`/`HLEN`; `HGETALL` returns a flat array `f1 v1 f2 v2` (compare as a map); `HINCRBY` on missing field creates it; `HINCRBY` on a non-integer field → error; `HDEL` last field removes the key.
- **Unit:** wrong type errors from every hash command when the key holds a string/list/set.
- **Refactor safety net:** all list/set tests from Tasks 14–15 still pass unchanged after you introduce the shared helper/trait. This is the point of having them.
- **Compile-time:** a `trybuild`-free check: adding a stub new value type (e.g. `Value::Stub`) requires only the new type's code plus an enum variant.

### Task 17: Sorted sets
- **Unit:** `ZADD z 1 a 1 b` ordering ties broken by member bytes (lexicographic); `ZADD` of an existing member updates the score and returns 0 new; `ZADD` with `NX|XX|GT|LT|CH` (stretch); non-float score → `-ERR value is not a valid float`; `nan` → error; `inf`/`-inf` accepted.
- **Unit:** `ZRANGE z 0 -1 WITHSCORES` output order and score formatting: `1`, `1.5`, `-0.1`, `inf`, `1e+20`? Compare with real Redis for formatting rules.
- **Unit:** `ZRANK` of missing member → nil; `ZSCORE` of missing → nil; `ZREM` last member deletes the key; `ZINCRBY` on missing creates.
- **Invariant test (important):** after N random `ZADD`/`ZREM`/`ZINCRBY` operations (xorshift-generated), assert `by_member.len() == by_score.len()` and that for every `(member, score)` in one index the other index has the same pair. Put this in a `#[cfg(test)] fn check_invariants(&self)` on the type and call it after each op in the test.
- **Unit:** `Score` newtype: `Ord` is consistent with `PartialOrd`/`Eq`; `-0.0` vs `0.0` treated the same (decide, and test it).
- **Differential:** `zsets.txt`.

---

## Phase E: Operability

### Task 18: Config, `SELECT`, `KEYS`
- **Unit (CLI):** parse args: defaults; `--port 7000`; invalid port → error exit code; `--help` output exists. With `clap`, use `Cli::try_parse_from([...])`.
- **Unit (glob), table-driven:**

| Pattern | Subject | Match |
|---|---|---|
| `*` | anything, `""` | yes |
| `h?llo` | `hello`, `hallo` | yes; `hllo`, `heello` | no |
| `h*llo` | `hllo`, `heeeello` | yes |
| `h[ae]llo` | `hello`, `hallo` | yes; `hillo` | no |
| `h[^e]llo` | `hallo` | yes; `hello` | no |
| `h[a-b]llo` | `hallo`, `hbllo` | yes; `hcllo` | no |
| `\*` | `*` | yes; `a` | no |
| `[` (unterminated) | anything | no panic (decide the result) |
| `**a**` | `xxaxx` | yes (no exponential blowup: also test `"a"*30 + "b"` against `"*a*a*a*a*a*c"` under 100 ms) |

- **Integration:** `SELECT 1`, `SET a 1`, `SELECT 0`, `GET a` → nil. `SELECT 99` → `-ERR DB index is out of range`. `FLUSHDB` clears only the selected db; `FLUSHALL` clears all. `DBSIZE` per db. Each connection has an independent selected db.
- **Integration:** `KEYS *` with 0, 1, 1000 keys; `SCAN 0 COUNT 10` loop until cursor `0` returns every key exactly once (when no concurrent writes).
- **Manual:** `redis-cli -n 3` works.

### Task 19: Logging and shutdown
- **Integration:** start the server through an API that returns a handle (`ServerHandle { shutdown(), join() }`). Connect a client, call `shutdown()`, and assert `join()` returns within 2 s even when a client is idle-connected.
- **Integration:** in-flight command completes: send a large pipeline, call `shutdown()`, and assert that you receive replies for every command *fully received* before the signal (and none are half-written).
- **Integration:** after shutdown, new connections are refused.
- **Unit:** `INFO` output contains `redis_version:`, `connected_clients:N`; `N` increments/decrements as clients connect/disconnect (poll with a timeout rather than sleeping blindly).
- **Manual:** Ctrl-C a running server: exit code 0, last log line is the shutdown message. `RUST_LOG=debug` shows command logs, `RUST_LOG=error` is quiet.
- **Leak check:** run 200 connect/disconnect cycles, then assert `connected_clients` returns to baseline.

### Task 20: Pub/Sub
- **Integration:** subscriber S on `news`, publisher P `PUBLISH news hi` → `:1`, S receives `["message","news","hi"]`. Subscribe confirmation is `["subscribe","news",1]`.
- **Integration:** 3 subscribers → `PUBLISH` returns `3`; all three receive the message. A subscriber to a different channel gets nothing (assert via read timeout).
- **Integration:** message ordering: publish 1000 messages; subscriber receives them in order.
- **Integration:** `UNSUBSCRIBE` → confirmation with the remaining count; subsequent `PUBLISH` returns 0.
- **Integration:** dropping the subscriber's TCP connection → registry is cleaned up (`PUBLISH` eventually returns 0).
- **Integration:** `PSUBSCRIBE news.*` matches `news.sports`, not `weather`; `PUBLISH` counts a client once per matching *subscription* as Redis does (check real Redis for a client subscribed to both a channel and a matching pattern).
- **Integration:** in subscribe mode `GET a` → error `-ERR Can't execute 'get': only (P)SUBSCRIBE / (P)UNSUBSCRIBE / PING / QUIT / RESET are allowed in this context`.
- **Edge:** publishing to a channel with no subscribers → `:0`, no allocation leak (registry doesn't keep empty entries).
- **Edge:** a slow subscriber (never reads) doesn't block publishers indefinitely. Decide: unbounded queue vs drop vs disconnect, and test your decision.

### Task 21: Transactions
- **Integration:** `MULTI` → OK; `SET a 1` → `QUEUED`; `INCR a` → `QUEUED`; `EXEC` → `["OK", 2]`; nothing visible to other clients until `EXEC` (check from a second client before `EXEC`).
- **Integration:** `DISCARD` drops the queue; `EXEC` afterwards → `-ERR EXEC without MULTI`. Nested `MULTI` → `-ERR MULTI calls can not be nested`.
- **Integration:** unknown command during queuing → error; `EXEC` → `-EXECABORT Transaction discarded because of previous errors.`
- **Integration:** runtime error inside EXEC (`INCR` on a non-integer) → that element is an error; others still apply.
- **Atomicity:** client A runs `MULTI; INCR x; INCR x; EXEC` ×1000 while client B concurrently runs `GET x` ×1000: B must never observe an odd value. Same with `SET`/`GET` pairs.
- **Disconnect:** closing a connection mid-`MULTI` leaves no state behind, and the next connection starts clean.
- **Stretch (`WATCH`):** key modified by another client between `WATCH` and `EXEC` → `EXEC` returns nil array and applies nothing; unmodified → applies.

---

## Phase F: Async

### Task 22: tokio port
- **Regression:** run *every* integration test from Tasks 8–21 unchanged against the async server. Only `tests/common` should change (and probably not even that, since it's still TCP).
- **Async unit tests:** `#[tokio::test]` for `Connection::read_value` using `tokio::io::duplex` as the stream, with the same chunking matrix as Task 6.
- **Concurrency:** 500 simultaneous connections each running 100 commands. With threads this strains the OS; with tasks it should be fast. Assert all complete in under N seconds.
- **Lock discipline:** enable the clippy lint `clippy::await_holding_lock` (on by default) and confirm it's quiet. Compile errors about `Send` are the compiler enforcing this.
- **Benchmark (manual):** `redis-benchmark -p 6380 -t set,get -n 100000 -c 50 -q` before/after; record both numbers in `docs/perf.md`.

### Task 23: Async pub/sub, blocking ops
- **`BLPOP` tests (`#[tokio::test]`, use `tokio::time::pause()` for deterministic time):**
  - Immediate: list non-empty → returns at once.
  - Blocks then wakes: `BLPOP q 0` waits; `RPUSH q x` from another client → the first gets `["q","x"]`.
  - Timeout: `BLPOP q 1` → nil array after ~1 s (use `advance(Duration::from_secs(1))`).
  - Multiple keys: `BLPOP a b 0`, push to `b` → returns `["b", v]`.
  - Fairness: two blocked clients on the same key; two pushes → first-blocked gets the first element (Redis serves in FIFO order).
  - Cancellation: the client disconnects while blocked; a later push goes to the *next* waiter or stays in the list. No lost elements. This is the cancellation-safety test, so make it a loop of 1000 randomized trials.
  - `MULTI` containing `BLPOP` doesn't block (Redis behaves as a non-blocking pop). Stretch.
- **Pub/Sub:** all Task 20 tests pass again. Add: a lagging `broadcast` receiver (`RecvError::Lagged`) is handled by your chosen policy (log + continue vs disconnect).
- **Shutdown:** with 100 idle clients plus 10 blocked on `BLPOP`, signal shutdown → everything exits within 1 s.
- **Static:** `grep -rn "thread::\(spawn\|sleep\)" src/` returns nothing.

---

## Phase G: Durability & extras

### Task 24: Snapshots
- **Unit (round-trip):** for each value type, empty and large (100k elements), binary keys/values, TTL'd keys: `load(dump(db)) == db`. Implement `PartialEq` on `Db` (or compare via a canonical dump).
- **Unit:** serialize into a `Vec<u8>`, decode from `Cursor`. No filesystem needed.
- **Corruption matrix:** for a valid dump: truncate at every byte offset (loop); flip each of a sample of bits; wrong magic; unknown version; unknown type tag; absurd length prefix (`u64::MAX`). Every case → `Err`, never a panic or a giant allocation.
- **Integration:** `SET`, `SAVE`, stop server, start new server on the same file, `GET` → value. Use `tempfile::tempdir()` for isolation.
- **Expiry across restart:** key with TTL 1 s; save; restart after 2 s → key is gone; restart after 0.2 s → TTL ≈ 0.8 s.
- **Atomicity:** kill the process during `SAVE` (simulate by failing the write halfway with a faulty `Write`) → the previous dump file is intact (temp-file + rename works).
- **`BGSAVE`:** writes keep working during a slow save (inject a delay); the saved snapshot reflects state at the moment `BGSAVE` was called, not later writes.

### Task 25: AOF
- **Unit:** each write command → expected log record (RESP array). Reads → nothing logged. Failed commands (WRONGTYPE) → not logged.
- **Unit (determinism):** `EXPIRE k 10` logs `PEXPIREAT k <abs>`; `SPOP` logs `SREM`; `INCRBYFLOAT` logs the resulting `SET` (if implemented).
- **Replay:** build a db through random operations; replay the AOF into an empty db; assert equal (reuse the `PartialEq` from Task 24).
- **Torn tail:** truncate the AOF at every byte offset of the last record → startup succeeds, drops the partial record, logs a warning, and the db equals the state without the last command.
- **Strict mode:** the same torn tail with truncation not allowed → startup fails and the file is left untouched.
- **Mid-file corruption:** bad bytes in the middle → startup fails with a clear error, not silent truncation of everything after.
- **Replay doesn't re-log:** the AOF size is unchanged after startup.
- **Integration:** `kill -9` test: spawn the real binary as a subprocess (`std::process::Command`), write 100 keys with `appendfsync always`, `kill -9`, restart, `DBSIZE` → 100.
- **fsync policy:** unit-test the policy state machine (`always` syncs per write; `everysec` at most once per second; `no` never) with a fake clock and a counting `Write`.

### Task 26: Performance
Tests here are about not regressing correctness while changing internals, plus measurement hygiene.

- **Full regression:** the whole suite passes after each optimization. Run it under `--release` too.
- **Sharding invariants:** `MSET`/`MGET`/`SINTER`/`EXEC` with keys deliberately spread across all shards, under 16 concurrent clients, never deadlock. Use a test timeout (a watchdog thread that fails after 10 s) so a deadlock is a failure, not a hang. Include opposite-order key pairs (`MSET a 1 b 2` vs `MSET b 1 a 2`) concurrently: the classic lock-ordering test.
- **Atomicity after sharding:** the `INCR` ×8000 and `EXEC` interleaving tests from Tasks 12 and 21 still pass.
- **Benchmarks (`benches/`, `criterion`):** `parse` and `encode` throughput on a representative mix; `Db::get`/`set` with 1M keys. Save the baseline (`cargo bench -- --save-baseline before`) and compare (`--baseline before`).
- **Allocation check (optional):** use `dhat` or a counting global allocator in a test to assert `parse` of a bulk string does ≤ 1 allocation.
- **End-to-end:** `redis-benchmark -q -t ping,set,get,incr,lpush,lpop -n 200000 -c 50 -P 16` against yours and real Redis; log in `docs/perf.md`.

### Task 27: Stretch goals
Choose tests for the options you pick:

- **Replication:** write 100 keys on the primary; `REPLICAOF` on a replica; poll until the replica `DBSIZE` is 100. Then writes on the primary appear on the replica within 1 s. Replica rejects writes (`-READONLY`). Replica reconnects after the primary restarts. `INFO replication` reports roles.
- **`HSCAN`/`SSCAN`/`ZSCAN`:** every element returned at least once across the cursor loop, including with concurrent inserts (elements present for the whole scan must be returned); cursor `0` terminates.
- **Bitmaps/HyperLogLog/Geo:** differential tests against real Redis, using values from the command docs' examples.
- **RESP3:** `HELLO 3` switches reply encoding; `HELLO 2` switches back; the parser handles every RESP3 type; clients that never send `HELLO` are unaffected.
- **Redis test suite subset:** clone `redis/redis`, run `./runtest --host 127.0.0.1 --port 6380 --single unit/type/list` (needs `tclsh`; `--host/--port` run against an external server). Track the pass count per file in `docs/compat.md`.
- **`unsafe` skiplist:** `cargo +nightly miri test` clean; a test that does random inserts/removes and compares against `BTreeMap`; `Drop` frees every node (count with a `Drop`-counting element type).

---

## Test-suite health checklist (review every ~5 tasks)

- [ ] `cargo test` completes in under ~20 s; timing-based tests have 3× margins.
- [ ] No test relies on a fixed port or on test order.
- [ ] Every user-visible error string is asserted at least once.
- [ ] Every `Value` type has a WRONGTYPE test for each command family.
- [ ] Differential scripts exist for strings, lists, sets, hashes, zsets, and pass against real Redis (modulo documented diffs listed in `tests/scripts/KNOWN_DIFFS.md`).
- [ ] `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` pass.
- [ ] Coverage spot check (optional): `cargo install cargo-llvm-cov && cargo llvm-cov --html`. Look for unexecuted error branches in the parser and connection code.
