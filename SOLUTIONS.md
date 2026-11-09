# SOLUTIONS: worked implementations of the tricky parts

**Spoiler warning.** Use this file after you have tried a task for a while and read the matching entry in [REFERENCES.md](REFERENCES.md). Read the "Where people get stuck" paragraph first. Often the idea is enough and you can write the code yourself.

## How this was checked

Every snippet below comes from one scratch crate (edition 2024, Rust 1.99) that compiles with `cargo clippy --all-targets` clean, is `rustfmt`-clean, and passes 72 tests, including the ones shown here. I ran the suite repeatedly (and in `--release`) to look for flaky timing. The only dependencies are `tokio` and `bytes`, and only for the async sections.

What that does and doesn't establish: the code works against its own tests. I have **not** run it under `redis-cli` or `redis-benchmark`, and the Redis edge-case behaviour it mirrors (for example how strict integer parsing is, or how scores are formatted) comes from my knowledge of Redis, so check it with the differential testing described in [TESTS.md](TESTS.md).

The snippets form one coherent program. A suggested module list for `src/lib.rs`:

```rust
pub mod resp;       // Task 4-5   Value, encode, parse
pub mod conn;       // Task 6     Connection<S>
pub mod util;       // Task 12/14 index_range, parse_i64_strict
pub mod command;    // Task 7     Command, TryFrom<Value>
pub mod db;         // Task 9-16  Db, expiry, watchers
pub mod exec;       // Task 10/21 execute(), Session (MULTI/EXEC/WATCH)
pub mod glob;       // Task 18
pub mod zset;       // Task 17
pub mod server;     // Task 2/10/19
pub mod pubsub;     // Task 20
pub mod aof;        // Task 25
pub mod snapshot;   // Task 24
pub mod async_server; // Task 22/23 (needs tokio + bytes)
```

Where "rsedis" is mentioned, it means the code in [seppo0010/rsedis](https://github.com/seppo0010/rsedis) as I read it for this file. Paths are real (I cloned the repo). Behaviour I only inferred from reading is marked "looks like" or "appears".

## Index

| # | Topic | Tasks |
|---|---|---|
| 1 | [RESP parser: `Incomplete` vs `Invalid`](#1-resp-parser-incomplete-vs-invalid) | 4, 5 |
| 2 | [Connection loop: fragments and pipelining](#2-connection-loop-fragments-and-pipelining) | 6 |
| 3 | [Command parsing with slice patterns, strict integers](#3-command-parsing-with-slice-patterns-strict-integers) | 7, 12 |
| 4 | [The db: borrow-checker patterns, expiry, `with_list`](#4-the-db-borrow-checker-patterns-expiry-with_list) | 9, 13, 14, 16 |
| 5 | [Lock scope and the executor](#5-lock-scope-and-the-executor) | 10, 11 |
| 6 | [Sorted set: `Ord` for floats and two indexes](#6-sorted-set-ord-for-floats-and-two-indexes) | 17 |
| 7 | [Glob matching without recursion](#7-glob-matching-without-recursion) | 18 |
| 8 | [Threaded server and graceful shutdown](#8-threaded-server-and-graceful-shutdown) | 2, 19 |
| 9 | [Pub/Sub with a writer thread](#9-pubsub-with-a-writer-thread) | 20 |
| 10 | [`MULTI`/`EXEC`/`WATCH`](#10-multiexecwatch) | 21 |
| 11 | [Async: the same parser, `BLPOP`, shutdown](#11-async-the-same-parser-blpop-shutdown) | 22, 23 |
| 12 | [Snapshots that survive corruption](#12-snapshots-that-survive-corruption) | 24 |
| 13 | [AOF: torn writes and deterministic logging](#13-aof-torn-writes-and-deterministic-logging) | 25 |
| 14 | [Trap checklist](#14-trap-checklist) | all |

---

## 1. RESP parser: `Incomplete` vs `Invalid`

**Where people get stuck.** The first parser usually returns `Err` for anything it cannot finish, so a request split across two TCP reads looks like a protocol error. The fix is a two-way failure type: `Incomplete` means "valid so far, read more bytes and call me again"; `Invalid` means "no future bytes can fix this". Everything else follows from that split.

Things the implementation below is careful about:
- **Length-prefixed means never scan the payload.** A bulk string may contain `\r\n` or NUL bytes, so the parser jumps `n` bytes and then checks for the CRLF.
- **Don't trust lengths.** `*1048576\r\n` must not reserve a million slots (`with_capacity(n.min(1024))`), and a client must not make you buffer forever without ever sending a CRLF (`MAX_LINE`). Nesting is capped so `*1\r\n*1\r\n...` cannot overflow the stack.
- **Reject an unknown type byte immediately**, before looking for a CRLF, otherwise `garbage\r\n`-style input hangs waiting for a terminator that may never arrive.

**rsedis.** `parser/src/lib.rs` does the same job differently. It only parses requests (arrays of bulk strings) and returns offsets, `Argument { pos, len }`, into the input buffer instead of copying: `ParsedCommand<'a>` borrows the buffer and `OwnedParsedCommand` owns it. That is faster and a great lifetimes exercise, but the reply side (`response/src/lib.rs`) is a separate `Response` enum, and the parser has no general `Value`. It also caps the argument count at 1024*1024 and bulk length at 512 MB. The two designs trade simplicity (this file) against allocation count (rsedis).

```rust
use std::io::Write;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Simple(String),
    Error(String),
    Integer(i64),
    Bulk(Option<Vec<u8>>),
    Array(Option<Vec<Value>>),
}

impl Value {
    pub fn ok() -> Value {
        Value::Simple("OK".into())
    }

    pub fn bulk(b: impl Into<Vec<u8>>) -> Value {
        Value::Bulk(Some(b.into()))
    }

    pub fn nil() -> Value {
        Value::Bulk(None)
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        // Writing into a Vec<u8> cannot fail, hence the expects.
        match self {
            Value::Simple(s) => write!(out, "+{s}\r\n").expect("vec write"),
            Value::Error(s) => write!(out, "-{s}\r\n").expect("vec write"),
            Value::Integer(i) => write!(out, ":{i}\r\n").expect("vec write"),
            Value::Bulk(None) => out.extend_from_slice(b"$-1\r\n"),
            Value::Bulk(Some(b)) => {
                write!(out, "${}\r\n", b.len()).expect("vec write");
                out.extend_from_slice(b);
                out.extend_from_slice(b"\r\n");
            }
            Value::Array(None) => out.extend_from_slice(b"*-1\r\n"),
            Value::Array(Some(items)) => {
                write!(out, "*{}\r\n", items.len()).expect("vec write");
                for item in items {
                    item.encode(out);
                }
            }
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum ParseError {
    /// Valid so far, but the buffer ends mid-frame. Read more and retry.
    Incomplete,
    /// Can never become valid, whatever bytes follow.
    Invalid(String),
}

const MAX_BULK: i64 = 512 * 1024 * 1024;
const MAX_ARRAY: i64 = 1024 * 1024;
const MAX_LINE: usize = 64 * 1024;
const MAX_DEPTH: usize = 32;

/// Parses one frame from the front of `buf`. Returns the value and the number
/// of bytes consumed.
pub fn parse(buf: &[u8]) -> Result<(Value, usize), ParseError> {
    parse_at(buf, 0, 0)
}

/// Returns the line starting at `pos` (without CRLF) and the position after the CRLF.
fn read_line(buf: &[u8], pos: usize) -> Result<(&[u8], usize), ParseError> {
    let rest = &buf[pos..];
    match rest.windows(2).position(|w| w == b"\r\n") {
        Some(i) => Ok((&rest[..i], pos + i + 2)),
        // No terminator yet. Without a cap, a client could make us buffer forever.
        None if rest.len() > MAX_LINE => Err(ParseError::Invalid("line too long".into())),
        None => Err(ParseError::Incomplete),
    }
}

fn parse_int(line: &[u8]) -> Result<i64, ParseError> {
    std::str::from_utf8(line)
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .ok_or_else(|| ParseError::Invalid("bad integer".into()))
}

fn parse_at(buf: &[u8], pos: usize, depth: usize) -> Result<(Value, usize), ParseError> {
    let kind = *buf.get(pos).ok_or(ParseError::Incomplete)?;
    // Reject an unknown type byte immediately instead of waiting for a CRLF.
    if !matches!(kind, b'+' | b'-' | b':' | b'$' | b'*') {
        return Err(ParseError::Invalid(format!("unknown type byte {kind:#x}")));
    }
    let (line, next) = read_line(buf, pos + 1)?;
    match kind {
        b'+' | b'-' => {
            let s = String::from_utf8(line.to_vec())
                .map_err(|_| ParseError::Invalid("non-utf8 line".into()))?;
            let v = if kind == b'+' {
                Value::Simple(s)
            } else {
                Value::Error(s)
            };
            Ok((v, next))
        }
        b':' => Ok((Value::Integer(parse_int(line)?), next)),
        b'$' => {
            let n = parse_int(line)?;
            if n == -1 {
                return Ok((Value::Bulk(None), next));
            }
            if !(0..=MAX_BULK).contains(&n) {
                return Err(ParseError::Invalid("bad bulk length".into()));
            }
            let end = next + n as usize;
            // Length-prefixed: never scan the payload for CRLF, it may contain one.
            if buf.len() < end + 2 {
                return Err(ParseError::Incomplete);
            }
            if &buf[end..end + 2] != b"\r\n" {
                return Err(ParseError::Invalid("bulk not terminated by CRLF".into()));
            }
            Ok((Value::Bulk(Some(buf[next..end].to_vec())), end + 2))
        }
        b'*' => {
            let n = parse_int(line)?;
            if n == -1 {
                return Ok((Value::Array(None), next));
            }
            if !(0..=MAX_ARRAY).contains(&n) {
                return Err(ParseError::Invalid("bad array length".into()));
            }
            if depth >= MAX_DEPTH {
                return Err(ParseError::Invalid("nesting too deep".into()));
            }
            // Don't trust n for the allocation: "*1048576\r\n" alone would reserve a lot.
            let mut items = Vec::with_capacity((n as usize).min(1024));
            let mut cur = next;
            for _ in 0..n {
                let (item, after) = parse_at(buf, cur, depth + 1)?;
                items.push(item);
                cur = after;
            }
            Ok((Value::Array(Some(items)), cur))
        }
        _ => unreachable!("checked above"),
    }
}
```

Tests worth copying (`sample_values()` is a `Vec<Value>` with one of each row from the encode table in [TESTS.md](TESTS.md)):

```rust
#[test]
fn every_proper_prefix_is_incomplete() {
    for v in sample_values() {
        let bytes = enc(&v);
        for i in 0..bytes.len() {
            assert_eq!(
                parse(&bytes[..i]),
                Err(ParseError::Incomplete),
                "{v:?} prefix {i}"
            );
        }
    }
}

#[test]
fn invalid_inputs() {
    let cases: &[&[u8]] = &[
        b"?foo\r\n",
        b"$abc\r\n",
        b"$-2\r\n",
        b"*-5\r\n",
        b":12x\r\n",
        b":\r\n",
        b"$3\r\nfoobar\r\n",
        b":99999999999999999999\r\n",
        b"$9999999999999\r\n",
    ];
    for c in cases {
        assert!(
            matches!(parse(c), Err(ParseError::Invalid(_))),
            "{:?}",
            String::from_utf8_lossy(c)
        );
    }
}

#[test]
fn deep_nesting_is_rejected_not_overflowed() {
    let mut buf = Vec::new();
    for _ in 0..10_000 {
        buf.extend_from_slice(b"*1\r\n");
    }
    assert!(matches!(parse(&buf), Err(ParseError::Invalid(_))));
}
```

The "every proper prefix is `Incomplete`" test is the one that catches most parser bugs.

---

## 2. Connection loop: fragments and pipelining

**Where people get stuck.** Two mistakes are common: parsing only once per `read` (so the second pipelined request waits forever for another read), and calling `read` before trying to parse (so a request already in the buffer is not served until the client sends more). The loop must be *parse, then read if incomplete, then parse again*.

Making the connection generic over `S: Read + Write` is what lets you test every possible TCP chunking without a socket.

Known cost: on a large bulk value arriving in many chunks, each loop re-parses from the start, which is O(n²). rsedis has the same weakness and says so (`// FIXME: is_incomplete parses the command a second time` in `networking/src/lib.rs`). Fixing it means remembering how many bytes the parser needs; leave that for the performance task.

```rust
use std::io::{self, Read, Write};

use crate::resp::{ParseError, Value, parse};

/// Generic over the stream so tests can feed bytes in any chunking.
pub struct Connection<S> {
    stream: S,
    buf: Vec<u8>,
}

impl<S: Read + Write> Connection<S> {
    pub fn new(stream: S) -> Self {
        Connection {
            stream,
            buf: Vec::new(),
        }
    }

    /// `Ok(None)` means the peer closed cleanly between frames.
    pub fn read_value(&mut self) -> io::Result<Option<Value>> {
        loop {
            // Parse first: a previous read may have delivered several pipelined frames.
            match parse(&self.buf) {
                Ok((value, used)) => {
                    self.buf.drain(..used);
                    return Ok(Some(value));
                }
                Err(ParseError::Incomplete) => {}
                Err(ParseError::Invalid(msg)) => {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, msg));
                }
            }
            let mut tmp = [0u8; 4096];
            let n = self.stream.read(&mut tmp)?;
            if n == 0 {
                return if self.buf.is_empty() {
                    Ok(None)
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "peer closed mid-frame",
                    ))
                };
            }
            self.buf.extend_from_slice(&tmp[..n]);
        }
    }

    pub fn write_value(&mut self, v: &Value) -> io::Result<()> {
        let mut out = Vec::new();
        v.encode(&mut out);
        self.stream.write_all(&out)
    }

    pub fn buffered(&self) -> usize {
        self.buf.len()
    }
}
```

Test with a fake stream that hands out at most `chunk` bytes per `read`:

```rust
/// Reads from `input` at most `chunk` bytes per call; records writes.
struct Chunked {
    input: Cursor<Vec<u8>>,
    chunk: usize,
    written: Vec<u8>,
}
impl Read for Chunked {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = buf.len().min(self.chunk);
        self.input.read(&mut buf[..n])
    }
}
impl Write for Chunked {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.written.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
```

```rust
#[test]
fn same_result_for_every_chunking() {
    let mut all = Vec::new();
    let values = crate::resp::tests::sample_values();
    for v in &values {
        v.encode(&mut all);
    }
    for chunk in [1, 2, 3, 7, 4096] {
        let mut c = conn(&all, chunk);
        for v in &values {
            assert_eq!(c.read_value().unwrap().as_ref(), Some(v), "chunk={chunk}");
        }
        assert_eq!(c.read_value().unwrap(), None);
    }
}

#[test]
fn eof_mid_frame_is_an_error_not_a_hang() {
    let mut c = conn(b"*2\r\n$3\r\nfoo\r\n$3\r\nba", 4096);
    let err = c.read_value().unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}
```

---

## 3. Command parsing with slice patterns, strict integers

**Where people get stuck.** Parsing `Vec<Vec<u8>>` into a typed enum with `if args.len() == 2 && args[0] == ...` chains gets messy fast. Matching on `(name.as_slice(), rest)` does the command lookup, the arity check and the destructuring in one `match`, and a final catch-all arm turns "known command, wrong arity" into the right Redis error. Notes:

- Byte-string literals like `b"PING"` work as patterns against `&[u8]`.
- Case-insensitivity is `to_ascii_uppercase()` on the name only. Echo the name the way the client typed it in "unknown command" errors.
- `str::parse::<i64>` is **more lenient than Redis**: it accepts `"+1"`. `parse_i64_strict` rejects `+1`, leading zeros, `-0`, spaces and overflow. `DECRBY k -9223372036854775808` would overflow on negation, so use `checked_neg`.
- Caps on `EX`/`PX` keep `Instant + Duration` from panicking later. Real Redis allows larger values.

rsedis does the equivalent in `database/src/dbutil.rs` (`vec_to_usize` rejects `"01"`), and parses command arguments lazily from the `ParsedCommand` with helpers like `get_vec(1)` and `get_i64(2)` inside one very large `match` in `command/src/command.rs`.

```rust
use std::time::Duration;

use crate::resp::Value;
use crate::util::parse_i64_strict;

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Ping(Option<Vec<u8>>),
    Echo(Vec<u8>),
    Get(Vec<u8>),
    Set {
        key: Vec<u8>,
        value: Vec<u8>,
        nx: bool,
        xx: bool,
        ttl: Option<Duration>,
    },
    Del(Vec<Vec<u8>>),
    IncrBy(Vec<u8>, i64),
    LPush(Vec<u8>, Vec<Vec<u8>>),
    LPop(Vec<u8>),
    LRange(Vec<u8>, i64, i64),
    PExpire(Vec<u8>, u64),
    Multi,
    Exec,
    Discard,
    Watch(Vec<Vec<u8>>),
    Unwatch,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CommandError {
    Unknown(String),
    WrongArity(String),
    Syntax,
    NotInteger,
    InvalidExpire(String),
    Protocol,
}

impl From<CommandError> for Value {
    fn from(e: CommandError) -> Value {
        let msg = match e {
            CommandError::Unknown(n) => format!("ERR unknown command '{n}'"),
            CommandError::WrongArity(n) => {
                format!("ERR wrong number of arguments for '{n}' command")
            }
            CommandError::Syntax => "ERR syntax error".to_string(),
            CommandError::NotInteger => "ERR value is not an integer or out of range".to_string(),
            CommandError::InvalidExpire(n) => format!("ERR invalid expire time in '{n}' command"),
            CommandError::Protocol => {
                "ERR Protocol error: expected array of bulk strings".to_string()
            }
        };
        Value::Error(msg)
    }
}

const KNOWN: &[&[u8]] = &[
    b"PING", b"ECHO", b"GET", b"SET", b"DEL", b"INCR", b"DECR", b"INCRBY", b"DECRBY", b"LPUSH",
    b"LPOP", b"LRANGE", b"PEXPIRE", b"MULTI", b"EXEC", b"DISCARD", b"WATCH", b"UNWATCH",
];

// Keeps `Instant + Duration` panic-free; real Redis allows larger values.
const MAX_EX_SECS: i64 = 1_000_000_000;
const MAX_PX_MILLIS: i64 = 1_000_000_000_000;

/// A request is an array of bulk strings. Anything else is a protocol error.
pub fn into_args(v: Value) -> Result<Vec<Vec<u8>>, CommandError> {
    let Value::Array(Some(items)) = v else {
        return Err(CommandError::Protocol);
    };
    items
        .into_iter()
        .map(|i| match i {
            Value::Bulk(Some(b)) => Ok(b),
            _ => Err(CommandError::Protocol),
        })
        .collect()
}

impl TryFrom<Value> for Command {
    type Error = CommandError;
    fn try_from(v: Value) -> Result<Command, CommandError> {
        Command::from_args(&into_args(v)?)
    }
}

fn int(b: &[u8]) -> Result<i64, CommandError> {
    parse_i64_strict(b).ok_or(CommandError::NotInteger)
}

impl Command {
    pub fn from_args(args: &[Vec<u8>]) -> Result<Command, CommandError> {
        let (name, rest) = args.split_first().ok_or(CommandError::Syntax)?;
        let upper = name.to_ascii_uppercase();
        // Slice patterns do the arity check and the destructuring in one go.
        Ok(match (upper.as_slice(), rest) {
            (b"PING", []) => Command::Ping(None),
            (b"PING", [m]) => Command::Ping(Some(m.clone())),
            (b"ECHO", [m]) => Command::Echo(m.clone()),
            (b"GET", [k]) => Command::Get(k.clone()),
            (b"SET", [k, v, opts @ ..]) => parse_set(k, v, opts)?,
            (b"DEL", keys) if !keys.is_empty() => Command::Del(keys.to_vec()),
            (b"INCR", [k]) => Command::IncrBy(k.clone(), 1),
            (b"DECR", [k]) => Command::IncrBy(k.clone(), -1),
            (b"INCRBY", [k, d]) => Command::IncrBy(k.clone(), int(d)?),
            // -i64::MIN does not exist: reject instead of overflowing.
            (b"DECRBY", [k, d]) => Command::IncrBy(
                k.clone(),
                int(d)?.checked_neg().ok_or(CommandError::NotInteger)?,
            ),
            (b"LPUSH", [k, vals @ ..]) if !vals.is_empty() => {
                Command::LPush(k.clone(), vals.to_vec())
            }
            (b"LPOP", [k]) => Command::LPop(k.clone()),
            (b"LRANGE", [k, a, b]) => Command::LRange(k.clone(), int(a)?, int(b)?),
            (b"PEXPIRE", [k, ms]) => {
                let ms = int(ms)?;
                Command::PExpire(k.clone(), ms.clamp(0, MAX_PX_MILLIS) as u64)
            }
            (b"MULTI", []) => Command::Multi,
            (b"EXEC", []) => Command::Exec,
            (b"DISCARD", []) => Command::Discard,
            (b"WATCH", keys) if !keys.is_empty() => Command::Watch(keys.to_vec()),
            (b"UNWATCH", []) => Command::Unwatch,
            (n, _) if KNOWN.contains(&n) => {
                return Err(CommandError::WrongArity(
                    String::from_utf8_lossy(n).to_lowercase(),
                ));
            }
            _ => {
                return Err(CommandError::Unknown(
                    String::from_utf8_lossy(name).into_owned(),
                ));
            }
        })
    }
}

fn parse_set(key: &[u8], value: &[u8], opts: &[Vec<u8>]) -> Result<Command, CommandError> {
    let (mut nx, mut xx, mut ttl) = (false, false, None);
    let mut it = opts.iter();
    // `while let` + `it.next()` lets an option consume its own argument.
    while let Some(opt) = it.next() {
        match opt.to_ascii_uppercase().as_slice() {
            b"NX" => nx = true,
            b"XX" => xx = true,
            o @ (b"EX" | b"PX") => {
                if ttl.is_some() {
                    return Err(CommandError::Syntax);
                }
                let n = int(it.next().ok_or(CommandError::Syntax)?)?;
                let (max, unit) = if o == b"EX" {
                    (MAX_EX_SECS, 1000)
                } else {
                    (MAX_PX_MILLIS, 1)
                };
                if n <= 0 || n > max {
                    return Err(CommandError::InvalidExpire("set".into()));
                }
                ttl = Some(Duration::from_millis(n as u64 * unit));
            }
            _ => return Err(CommandError::Syntax),
        }
    }
    if nx && xx {
        return Err(CommandError::Syntax);
    }
    Ok(Command::Set {
        key: key.to_vec(),
        value: value.to_vec(),
        nx,
        xx,
        ttl,
    })
}
```

```rust
/// Converts Redis-style `start`/`stop` (inclusive, negative = from the end)
/// into a half-open `start..end` that is always safe to slice with.
/// Returns `None` when the range is empty.
pub fn index_range(start: i64, stop: i64, len: usize) -> Option<std::ops::Range<usize>> {
    let len = len as i64;
    let mut start = if start < 0 { start + len } else { start };
    let mut stop = if stop < 0 { stop + len } else { stop };
    if start < 0 {
        start = 0;
    }
    if stop >= len {
        stop = len - 1;
    }
    // Also catches len == 0 (stop == -1) and start past the end.
    if start > stop || start >= len {
        return None;
    }
    Some(start as usize..stop as usize + 1)
}

/// Integer parsing as strict as Redis' `string2ll`: `str::parse` alone would
/// accept "+1", which Redis rejects. Leading zeros, "-0", spaces: all rejected.
pub fn parse_i64_strict(b: &[u8]) -> Option<i64> {
    let digits = b.strip_prefix(b"-").unwrap_or(b);
    match digits {
        [] => return None,
        [b'0'] if b.len() == 1 => return Some(0),
        [b'0', ..] => return None, // "01", "-0", "-01"
        _ => {}
    }
    if !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(b).ok()?.parse().ok() // overflow -> None
}
```

```rust
#[test]
fn lrange_table() {
    // list [a,b,c,d,e]
    type Case = (i64, i64, Option<(usize, usize)>);
    let cases: &[Case] = &[
        (0, -1, Some((0, 5))),
        (0, 0, Some((0, 1))),
        (-2, -1, Some((3, 5))),
        (1, 100, Some((1, 5))),
        (-100, 2, Some((0, 3))),
        (3, 1, None),
        (10, 20, None),
        (-1, -100, None),
    ];
    for &(s, e, want) in cases {
        assert_eq!(
            index_range(s, e, 5).map(|r| (r.start, r.end)),
            want,
            "{s} {e}"
        );
    }
    assert_eq!(index_range(0, -1, 0), None);
    assert_eq!(index_range(i64::MIN, i64::MAX, 3), Some(0..3));
}

#[test]
fn strict_ints() {
    let ok: &[(&[u8], i64)] = &[
        (b"0", 0),
        (b"7", 7),
        (b"-7", -7),
        (b"9223372036854775807", i64::MAX),
        (b"-9223372036854775808", i64::MIN),
    ];
    for &(s, v) in ok {
        assert_eq!(parse_i64_strict(s), Some(v));
    }
    for s in [
        &b""[..],
        b"-",
        b"+1",
        b" 1",
        b"1 ",
        b"01",
        b"-0",
        b"1.5",
        b"abc",
        b"9223372036854775808",
    ] {
        assert_eq!(
            parse_i64_strict(s),
            None,
            "{:?}",
            String::from_utf8_lossy(s)
        );
    }
}
```

```rust
#[test]
fn errors() {
    assert_eq!(
        parse(&["echo"]),
        Err(CommandError::WrongArity("echo".into()))
    );
    assert_eq!(
        parse(&["GET", "a", "b"]),
        Err(CommandError::WrongArity("get".into()))
    );
    assert_eq!(parse(&["foo"]), Err(CommandError::Unknown("foo".into())));
    assert_eq!(
        parse(&["set", "k", "v", "nx", "xx"]),
        Err(CommandError::Syntax)
    );
    assert_eq!(parse(&["set", "k", "v", "ex"]), Err(CommandError::Syntax));
    assert_eq!(
        parse(&["set", "k", "v", "ex", "10", "px", "5"]),
        Err(CommandError::Syntax)
    );
    assert!(matches!(
        parse(&["set", "k", "v", "ex", "0"]),
        Err(CommandError::InvalidExpire(_))
    ));
    assert_eq!(
        parse(&["set", "k", "v", "ex", "abc"]),
        Err(CommandError::NotInteger)
    );
    assert_eq!(parse(&["incrby", "k", "+1"]), Err(CommandError::NotInteger));
    assert_eq!(
        parse(&["decrby", "k", "-9223372036854775808"]),
        Err(CommandError::NotInteger)
    );
}
```

---

## 4. The db: borrow-checker patterns, expiry, `with_list`

Three separate problems live in `db.rs`.

**(a) Lazy expiry and the borrow checker.** You want "if the key is expired, delete it, then look it up". Writing `if let Some(e) = self.map.get(key) { if expired(e) { self.remove(key) } }` fails with E0502, because `e` borrows `self.map` while `remove` needs `&mut self`. The fix is to compute a plain `bool` first so the shared borrow ends:

```rust
let expired = self.map.get(key).and_then(|e| e.expires_at).is_some_and(|t| t <= now);
if expired { self.remove(key); }
```

Also note why `get_str(&mut self, ...) -> Result<Option<&[u8]>, _>` takes `&mut self` even though it "reads": it may delete. The returned slice borrows the db, so the caller cannot call `set` until it has dropped the slice. That is the behaviour the compiler error in Task 9 is teaching.

**(b) Active expiry without random sampling.** Redis (and rsedis) sample random keys from the expiry table because a hash table has no order. In Rust you can do better cheaply: keep a `BTreeSet<(Instant, key)>`; the sweeper only ever looks at the front, so it is exact and O(log n) per key. The price is an invariant you must protect: `deadlines` has exactly one entry per key that has an `expires_at`. That is why *every* removal goes through the private `remove`, and why `set_expiry` removes the old pair before inserting the new one. The test below checks it after overwrites, re-expiry and deletion.

rsedis, for comparison (`database/src/lib.rs`): it keeps a separate `data_expiration_ms` map of wall-clock milliseconds (`mstime()`), checks `is_expired` on each access, and `active_expire_cycle` samples random keys with a `random_key!` macro, repeating while more than a quarter of the sample was expired, with a time-budget check every few iterations. A background "hz" thread locks the database, runs one cycle, drops the lock and sleeps. Two details: the loop bound reads `self.data_expiration_ms.len()`, which looks like the number of databases rather than the number of expiring keys in this database, so I would not copy it blindly; and `is_expired` returns false while `loading` is set, so replaying a log does not delete keys mid-load.

**(c) Time you can control.** `Clock` is a tiny trait with `SystemClock` and a `ManualClock` for tests. Without it, expiry tests need `sleep` and are slow or flaky. `Arc<dyn Clock>` is the trait-object-vs-generic decision from Task 16: it keeps `Db` non-generic, which matters because `Db` ends up inside `Arc<Mutex<..>>` everywhere.

**(d) `with_list`: one place for "get, type-check, mutate, delete if empty".** Without it, every list command repeats four steps and one of them forgets to delete the empty list. Passing the mutation as an `FnOnce` closure lets the helper own the lifecycle. Look at the line `let now_empty = list.is_empty();`: it is the last use of `list`, so the mutable borrow ends and `self.remove(key)` compiles.

`index_range` (above, in `util.rs`) is the Redis index rule (negative from the end, clamped, empty when start > stop) in one tested function. rsedis uses `normalize_position(pos, len) -> Result<usize, bool>` where the `bool` says which side of the list you fell off, and `lrange` then handles the four combinations by hand; the single-range version is easier to get right.

```rust
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::util::parse_i64_strict;

// ---- Clock: lets tests control time ------------------------------------

pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

pub struct ManualClock {
    base: Instant,
    offset_ms: AtomicU64,
}
impl ManualClock {
    pub fn new() -> Arc<ManualClock> {
        Arc::new(ManualClock {
            base: Instant::now(),
            offset_ms: AtomicU64::new(0),
        })
    }
    pub fn advance(&self, d: Duration) {
        self.offset_ms
            .fetch_add(d.as_millis() as u64, Ordering::SeqCst);
    }
}
impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.base + Duration::from_millis(self.offset_ms.load(Ordering::SeqCst))
    }
}

// ---- Watchers: optimistic locking for WATCH/EXEC -----------------------

/// Which clients watch which keys, and which of them saw a modification.
/// Redis does the same: writes flag watchers "dirty"; EXEC refuses if dirty.
#[derive(Default)]
pub struct Watchers {
    by_key: HashMap<Vec<u8>, HashSet<u64>>,
    dirty: HashSet<u64>,
}

impl Watchers {
    pub fn watch(&mut self, client: u64, key: &[u8]) {
        self.by_key.entry(key.to_vec()).or_default().insert(client);
    }

    /// Called by every write path.
    pub fn touch(&mut self, key: &[u8]) {
        if let Some(clients) = self.by_key.remove(key) {
            self.dirty.extend(clients);
        }
    }

    /// Ends the client's watches (EXEC, DISCARD, UNWATCH, disconnect).
    /// Returns true if none of the watched keys changed.
    pub fn finish(&mut self, client: u64, keys: &[Vec<u8>]) -> bool {
        for k in keys {
            if let Some(set) = self.by_key.get_mut(k) {
                set.remove(&client);
                if set.is_empty() {
                    self.by_key.remove(k);
                }
            }
        }
        !self.dirty.remove(&client)
    }
}

// ---- Db ----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Data {
    Str(Vec<u8>),
    List(VecDeque<Vec<u8>>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub data: Data,
    pub expires_at: Option<Instant>,
}

#[derive(Debug, PartialEq)]
pub struct WrongType;

#[derive(Debug, PartialEq)]
pub enum IncrError {
    WrongType,
    NotInteger,
    Overflow,
}

pub struct Db {
    map: HashMap<Vec<u8>, Entry>,
    /// Invariant: contains exactly one (deadline, key) per entry with `expires_at`.
    /// Ordered, so the sweeper only ever looks at the front.
    deadlines: BTreeSet<(Instant, Vec<u8>)>,
    clock: Arc<dyn Clock>,
    pub watchers: Watchers,
}

impl Db {
    pub fn new(clock: Arc<dyn Clock>) -> Db {
        Db {
            map: HashMap::new(),
            deadlines: BTreeSet::new(),
            clock,
            watchers: Watchers::default(),
        }
    }

    /// Number of stored entries, including expired-but-not-yet-swept ones.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    // Every removal goes through here so `deadlines` and `watchers` stay in sync.
    fn remove(&mut self, key: &[u8]) -> Option<Entry> {
        let entry = self.map.remove(key)?;
        if let Some(t) = entry.expires_at {
            self.deadlines.remove(&(t, key.to_vec()));
        }
        self.watchers.touch(key);
        Some(entry)
    }

    /// Lazy expiry. Compute the bool first so the shared borrow of `self.map`
    /// ends before `self.remove` needs `&mut self`.
    fn purge_if_expired(&mut self, key: &[u8]) {
        let now = self.clock.now();
        let expired = self
            .map
            .get(key)
            .and_then(|e| e.expires_at)
            .is_some_and(|t| t <= now);
        if expired {
            self.remove(key);
        }
    }

    /// Active expiry: pop deadlines from the front while they are due.
    /// (rsedis, like Redis, samples random keys instead; see the notes.)
    pub fn sweep(&mut self) -> usize {
        let now = self.clock.now();
        let mut removed = 0;
        while self.deadlines.first().is_some_and(|(t, _)| *t <= now) {
            if let Some((_, key)) = self.deadlines.pop_first() {
                self.map.remove(&key);
                self.watchers.touch(&key);
                removed += 1;
            }
        }
        removed
    }

    pub fn exists(&mut self, key: &[u8]) -> bool {
        self.purge_if_expired(key);
        self.map.contains_key(key)
    }

    pub fn del(&mut self, key: &[u8]) -> bool {
        self.purge_if_expired(key);
        self.remove(key).is_some()
    }

    /// Returns a borrow tied to `&mut self`; the caller cannot touch the db again
    /// until it is done with the slice. That is the borrow checker doing its job.
    pub fn get_str(&mut self, key: &[u8]) -> Result<Option<&[u8]>, WrongType> {
        self.purge_if_expired(key);
        match self.map.get(key) {
            None => Ok(None),
            Some(Entry {
                data: Data::Str(s), ..
            }) => Ok(Some(s)),
            Some(_) => Err(WrongType),
        }
    }

    /// SET semantics: replaces any type and clears any TTL. Returns false if NX/XX blocked it.
    pub fn set(
        &mut self,
        key: &[u8],
        value: Vec<u8>,
        nx: bool,
        xx: bool,
        ttl: Option<Duration>,
    ) -> bool {
        self.purge_if_expired(key);
        let present = self.map.contains_key(key);
        if (nx && present) || (xx && !present) {
            return false;
        }
        self.remove(key);
        self.map.insert(
            key.to_vec(),
            Entry {
                data: Data::Str(value),
                expires_at: None,
            },
        );
        self.watchers.touch(key);
        if let Some(ttl) = ttl {
            self.set_expiry(key, ttl);
        }
        true
    }

    pub fn set_expiry(&mut self, key: &[u8], ttl: Duration) -> bool {
        self.purge_if_expired(key);
        let deadline = self.clock.now() + ttl;
        let Some(entry) = self.map.get_mut(key) else {
            return false;
        };
        if let Some(old) = entry.expires_at.replace(deadline) {
            self.deadlines.remove(&(old, key.to_vec()));
        }
        self.deadlines.insert((deadline, key.to_vec()));
        self.watchers.touch(key);
        true
    }

    /// Redis PTTL: -2 missing, -1 no expiry, else remaining milliseconds.
    pub fn pttl(&mut self, key: &[u8]) -> i64 {
        self.purge_if_expired(key);
        match self.map.get(key) {
            None => -2,
            Some(Entry {
                expires_at: None, ..
            }) => -1,
            Some(Entry {
                expires_at: Some(t),
                ..
            }) => t.saturating_duration_since(self.clock.now()).as_millis() as i64,
        }
    }

    /// Read-modify-write in one `&mut self` call, so under the db lock it is atomic.
    pub fn incr_by(&mut self, key: &[u8], delta: i64) -> Result<i64, IncrError> {
        self.purge_if_expired(key);
        let current = match self.map.get(key) {
            None => 0,
            Some(Entry {
                data: Data::Str(s), ..
            }) => parse_i64_strict(s).ok_or(IncrError::NotInteger)?,
            Some(_) => return Err(IncrError::WrongType),
        };
        let next = current.checked_add(delta).ok_or(IncrError::Overflow)?;
        match self.map.get_mut(key) {
            // Keep the TTL: INCR does not clear it.
            Some(entry) => entry.data = Data::Str(next.to_string().into_bytes()),
            None => {
                self.map.insert(
                    key.to_vec(),
                    Entry {
                        data: Data::Str(next.to_string().into_bytes()),
                        expires_at: None,
                    },
                );
            }
        }
        self.watchers.touch(key);
        Ok(next)
    }

    /// Runs `f` on the list at `key`.
    /// - `create`: make an empty list if the key is missing, otherwise `Ok(None)`.
    /// - Removes the key afterwards if the list became empty (Redis semantics),
    ///   in one place instead of in every command.
    pub fn with_list<R>(
        &mut self,
        key: &[u8],
        create: bool,
        f: impl FnOnce(&mut VecDeque<Vec<u8>>) -> R,
    ) -> Result<Option<R>, WrongType> {
        self.purge_if_expired(key);
        if !self.map.contains_key(key) {
            if !create {
                return Ok(None);
            }
            self.map.insert(
                key.to_vec(),
                Entry {
                    data: Data::List(VecDeque::new()),
                    expires_at: None,
                },
            );
        }
        let Some(Entry {
            data: Data::List(list),
            ..
        }) = self.map.get_mut(key)
        else {
            return Err(WrongType);
        };
        let result = f(list);
        let now_empty = list.is_empty(); // last use of `list`: the borrow ends here
        if now_empty {
            self.remove(key);
        } else {
            self.watchers.touch(key);
        }
        Ok(Some(result))
    }

    pub fn list(&mut self, key: &[u8]) -> Result<Option<&VecDeque<Vec<u8>>>, WrongType> {
        self.purge_if_expired(key);
        match self.map.get(key) {
            None => Ok(None),
            Some(Entry {
                data: Data::List(l),
                ..
            }) => Ok(Some(l)),
            Some(_) => Err(WrongType),
        }
    }

    pub fn entries(&self) -> impl Iterator<Item = (&Vec<u8>, &Entry)> {
        self.map.iter()
    }

    pub fn now(&self) -> Instant {
        self.clock.now()
    }

    /// Raw insert used when loading snapshots / AOF.
    pub fn load_entry(&mut self, key: Vec<u8>, data: Data, expires_at: Option<Instant>) {
        self.remove(&key);
        if let Some(t) = expires_at {
            self.deadlines.insert((t, key.clone()));
        }
        self.map.insert(key, Entry { data, expires_at });
    }
}
```

```rust
#[test]
fn lazy_expiry_and_ttl() {
    let (mut db, clock) = db();
    db.set(
        b"k",
        b"v".to_vec(),
        false,
        false,
        Some(Duration::from_millis(100)),
    );
    assert_eq!(db.get_str(b"k"), Ok(Some(&b"v"[..])));
    assert_eq!(db.pttl(b"k"), 100);
    clock.advance(Duration::from_millis(60));
    assert_eq!(db.pttl(b"k"), 40);
    clock.advance(Duration::from_millis(40));
    assert_eq!(db.get_str(b"k"), Ok(None));
    assert_eq!(db.pttl(b"k"), -2);
    assert_eq!(db.len(), 0);
}

#[test]
fn active_sweep_removes_unread_keys_and_keeps_index_in_sync() {
    let (mut db, clock) = db();
    for i in 0..1000 {
        db.set(
            format!("k{i}").as_bytes(),
            vec![1],
            false,
            false,
            Some(Duration::from_millis(100)),
        );
    }
    db.set(b"keep", vec![1], false, false, None);
    assert_eq!(db.sweep(), 0);
    clock.advance(Duration::from_millis(100));
    assert_eq!(db.sweep(), 1000);
    assert_eq!((db.len(), db.deadlines.len()), (1, 0));
    // Re-expiring and overwriting must not leave stale deadlines behind.
    db.set(b"a", vec![1], false, false, Some(Duration::from_secs(5)));
    db.set_expiry(b"a", Duration::from_secs(10));
    assert_eq!(db.deadlines.len(), 1);
    db.set(b"a", vec![2], false, false, None);
    assert_eq!(db.deadlines.len(), 0);
    db.set(b"a", vec![2], false, false, Some(Duration::from_secs(5)));
    db.del(b"a");
    assert_eq!(db.deadlines.len(), 0);
}

#[test]
fn list_helper_deletes_empty_lists_and_checks_types() {
    let (mut db, _) = db();
    db.with_list(b"l", true, |l| {
        l.push_front(b"a".to_vec());
        l.push_front(b"b".to_vec());
    })
    .unwrap();
    assert_eq!(
        db.with_list(b"l", false, |l| l.pop_front()),
        Ok(Some(Some(b"b".to_vec())))
    );
    assert_eq!(
        db.with_list(b"l", false, |l| l.pop_front()),
        Ok(Some(Some(b"a".to_vec())))
    );
    assert!(!db.exists(b"l"), "empty list must delete the key");
    assert_eq!(db.with_list(b"l", false, |l| l.len()), Ok(None));
    db.set(b"s", vec![1], false, false, None);
    assert_eq!(db.with_list(b"s", true, |l| l.len()), Err(WrongType));
    assert_eq!(db.list(b"s"), Err(WrongType));
}
```

---

## 5. Lock scope and the executor

**Where people get stuck.** (1) Holding the `MutexGuard` while writing to the socket, so one slow client stalls everyone. (2) Making `INCR` a `get` then a `set` under two separate lock acquisitions. (3) Deciding what a poisoned mutex means.

The structure that avoids all three is: **`execute(&mut Db, Command) -> Value` is pure** (no I/O, no locking), and callers lock around it. `INCR` is then a single `&mut self` method, atomic for free, and `EXEC` (section 10) can call `execute` many times under one guard.

`lock()` recovers from poisoning on purpose: our mutations are single method calls that leave the maps consistent, so a panic in one connection thread should not take every other client down. If you prefer to fail hard, `unwrap()` is also defensible. Write down the choice.

rsedis scopes the lock the same way (`networking/src/lib.rs`, `Client::run`):

```rust
let r = {
    let mut db = match self.db.lock() { Ok(db) => db, Err(_) => break };
    command::command(parsed_command, &mut *db, &mut client)
};
```

and keeps slow clients from holding the lock by writing replies from a separate writer thread fed by a channel (see section 9).

```rust
// The Session half of this file (MULTI/EXEC/WATCH) is explained in section 10.
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::command::{Command, CommandError};
use crate::db::{Db, IncrError, WrongType};
use crate::resp::Value;
use crate::util::index_range;

/// Poisoning only means another thread panicked while holding the lock. Our
/// mutations are single calls that leave the maps consistent, so keep serving
/// instead of taking every client down. Write down whichever policy you pick.
pub fn lock(db: &Mutex<Db>) -> MutexGuard<'_, Db> {
    db.lock().unwrap_or_else(PoisonError::into_inner)
}

fn wrong_type() -> Value {
    Value::Error("WRONGTYPE Operation against a key holding the wrong kind of value".into())
}

impl From<IncrError> for Value {
    fn from(e: IncrError) -> Value {
        match e {
            IncrError::WrongType => wrong_type(),
            IncrError::NotInteger => CommandError::NotInteger.into(),
            IncrError::Overflow => Value::Error("ERR increment or decrement would overflow".into()),
        }
    }
}

/// Executes one command against an already-locked db. Pure: no I/O, no locking,
/// which is what lets EXEC run many of these under one lock acquisition.
pub fn execute(db: &mut Db, cmd: Command) -> Value {
    match cmd {
        Command::Ping(None) => Value::Simple("PONG".into()),
        Command::Ping(Some(m)) | Command::Echo(m) => Value::bulk(m),
        Command::Get(k) => match db.get_str(&k) {
            Ok(Some(v)) => Value::bulk(v),
            Ok(None) => Value::nil(),
            Err(WrongType) => wrong_type(),
        },
        Command::Set {
            key,
            value,
            nx,
            xx,
            ttl,
        } => {
            if db.set(&key, value, nx, xx, ttl) {
                Value::ok()
            } else {
                Value::nil()
            }
        }
        Command::Del(keys) => Value::Integer(keys.iter().filter(|k| db.del(k)).count() as i64),
        Command::IncrBy(k, d) => match db.incr_by(&k, d) {
            Ok(n) => Value::Integer(n),
            Err(e) => e.into(),
        },
        Command::LPush(k, vals) => match db.with_list(&k, true, |l| {
            for v in vals {
                l.push_front(v);
            }
            l.len() as i64
        }) {
            Ok(n) => Value::Integer(n.unwrap_or(0)),
            Err(WrongType) => wrong_type(),
        },
        Command::LPop(k) => match db.with_list(&k, false, |l| l.pop_front()) {
            Ok(Some(Some(v))) => Value::bulk(v),
            Ok(_) => Value::nil(),
            Err(WrongType) => wrong_type(),
        },
        Command::LRange(k, a, b) => match db.list(&k) {
            Ok(None) => Value::Array(Some(vec![])),
            Ok(Some(l)) => {
                let items = match index_range(a, b, l.len()) {
                    Some(r) => l
                        .iter()
                        .skip(r.start)
                        .take(r.len())
                        .map(|v| Value::bulk(v.clone()))
                        .collect(),
                    None => vec![],
                };
                Value::Array(Some(items))
            }
            Err(WrongType) => wrong_type(),
        },
        Command::PExpire(k, ms) => {
            Value::Integer(db.set_expiry(&k, Duration::from_millis(ms)) as i64)
        }
        Command::Multi
        | Command::Exec
        | Command::Discard
        | Command::Watch(_)
        | Command::Unwatch => Value::Error("ERR transaction command not allowed here".into()),
    }
}

// ---- Per-connection session: MULTI / EXEC / WATCH ------------------------

enum TxState {
    Normal,
    Queuing { queue: Vec<Command>, aborted: bool },
}

pub struct Session {
    id: u64,
    tx: TxState,
    watched: Vec<Vec<u8>>,
}

impl Session {
    pub fn new(id: u64) -> Session {
        Session {
            id,
            tx: TxState::Normal,
            watched: Vec::new(),
        }
    }

    /// Handle one parsed request. Locks only for as long as it must.
    pub fn handle(&mut self, db: &Mutex<Db>, parsed: Result<Command, CommandError>) -> Value {
        let err = |s: &str| Value::Error(s.to_string());
        match (&mut self.tx, parsed) {
            // ---- inside MULTI
            (TxState::Queuing { aborted, .. }, Err(e)) => {
                *aborted = true; // Redis: any queuing error dooms the transaction
                e.into()
            }
            (TxState::Queuing { .. }, Ok(Command::Multi)) => {
                err("ERR MULTI calls can not be nested")
            }
            (TxState::Queuing { .. }, Ok(Command::Watch(_))) => {
                err("ERR WATCH inside MULTI is not allowed")
            }
            (TxState::Queuing { .. }, Ok(Command::Exec)) => self.exec(db),
            (TxState::Queuing { .. }, Ok(Command::Discard)) => {
                self.tx = TxState::Normal;
                self.unwatch(db);
                Value::ok()
            }
            (TxState::Queuing { queue, .. }, Ok(cmd)) => {
                queue.push(cmd);
                Value::Simple("QUEUED".into())
            }
            // ---- outside MULTI
            (TxState::Normal, Err(e)) => e.into(),
            (TxState::Normal, Ok(Command::Multi)) => {
                self.tx = TxState::Queuing {
                    queue: Vec::new(),
                    aborted: false,
                };
                Value::ok()
            }
            (TxState::Normal, Ok(Command::Exec)) => err("ERR EXEC without MULTI"),
            (TxState::Normal, Ok(Command::Discard)) => err("ERR DISCARD without MULTI"),
            (TxState::Normal, Ok(Command::Watch(keys))) => {
                let mut g = lock(db);
                for k in &keys {
                    g.watchers.watch(self.id, k);
                }
                self.watched.extend(keys);
                Value::ok()
            }
            (TxState::Normal, Ok(Command::Unwatch)) => {
                self.unwatch(db);
                Value::ok()
            }
            (TxState::Normal, Ok(cmd)) => execute(&mut lock(db), cmd),
        }
    }

    fn unwatch(&mut self, db: &Mutex<Db>) -> bool {
        let keys = std::mem::take(&mut self.watched);
        lock(db).watchers.finish(self.id, &keys)
    }

    fn exec(&mut self, db: &Mutex<Db>) -> Value {
        let TxState::Queuing { queue, aborted } = std::mem::replace(&mut self.tx, TxState::Normal)
        else {
            unreachable!("exec() is only called while queuing")
        };
        // One lock acquisition for the dirty-check AND every queued command:
        // nothing can interleave, which is what "atomic" means here.
        let mut g = lock(db);
        let keys = std::mem::take(&mut self.watched);
        let clean = g.watchers.finish(self.id, &keys);
        if aborted {
            return Value::Error(
                "EXECABORT Transaction discarded because of previous errors.".into(),
            );
        }
        if !clean {
            return Value::Array(None);
        }
        Value::Array(Some(
            queue.into_iter().map(|c| execute(&mut g, c)).collect(),
        ))
    }

    /// Call when the connection closes so watchers don't leak.
    pub fn close(&mut self, db: &Mutex<Db>) {
        self.unwatch(db);
    }
}
```

```rust
#[test]
fn concurrent_incr_is_exact() {
    let db = Arc::new(Mutex::new(Db::new(Arc::new(SystemClock))));
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let db = db.clone();
            std::thread::spawn(move || {
                let mut s = Session::new(i);
                for _ in 0..1000 {
                    s.handle(&db, cmd(&["incr", "n"]));
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(
        execute(&mut lock(&db), cmd(&["get", "n"]).unwrap()),
        Value::bulk("8000")
    );
}
```

---

## 6. Sorted set: `Ord` for floats and two indexes

**Where people get stuck.** `BTreeSet<(f64, Vec<u8>)>` does not compile because `f64` is not `Ord` (NaN breaks the total order). Three decisions make it work:

1. A `Score(f64)` newtype that **cannot hold NaN** (the constructor returns `Option`) and implements `Ord` with `total_cmp`.
2. Normalise `-0.0` to `0.0`. Otherwise the derived `PartialEq` (`-0.0 == 0.0`) and `total_cmp` (which orders `-0.0 < 0.0`) disagree, which silently breaks `BTreeSet`.
3. Reject NaN at the *operation* level too: `inf + -inf` is NaN, so `ZINCRBY` returns an error and leaves the state unchanged.

The second trap is **two indexes drifting apart**. `ZADD` on an existing member must remove the *old* `(score, member)` from `order` before inserting the new one. Keep the fields private so only `add`/`remove` can touch them, and keep a test-only `check_invariants` that you call after every operation in a randomized test.

`rank` is O(n) here (counting entries before this one). Redis' skiplist stores span lengths for O(log n); that is a worthwhile upgrade later.

rsedis (`database/src/zset.rs`) is the same design: a `skiplist` crate `OrderedSkipList<SortedSetMember>` plus a `HashMap<Vec<u8>, f64>`, with `zadd` removing the old pair before inserting. Its `SortedSetMember` wraps `f64` and a `upper_boundary` flag, a trick for exclusive range bounds.

```rust
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};

/// An `f64` that is never NaN, so a total order exists. `-0.0` is normalised to
/// `0.0` so that `==` (used by the derived `PartialEq`) agrees with `total_cmp`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Score(f64);

impl Score {
    pub fn new(f: f64) -> Option<Score> {
        if f.is_nan() {
            None
        } else {
            Some(Score(if f == 0.0 { 0.0 } else { f }))
        }
    }
    pub fn get(self) -> f64 {
        self.0
    }
}

impl Eq for Score {}

impl PartialOrd for Score {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Score {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// Two indexes over the same data:
///
/// - `scores`: member -> score, O(1) lookup (ZSCORE, "does it exist")
/// - `order`:  (score, member), kept sorted; ties break by member bytes like Redis
///
/// Fields are private so no caller can update one without the other.
/// rsedis does the same with a skiplist + HashMap; Redis uses a skiplist + dict.
#[derive(Default)]
pub struct ZSet {
    scores: HashMap<Vec<u8>, Score>,
    order: BTreeSet<(Score, Vec<u8>)>,
}

#[derive(Debug, PartialEq)]
pub struct NanScore;

impl ZSet {
    pub fn len(&self) -> usize {
        self.scores.len()
    }

    pub fn is_empty(&self) -> bool {
        self.scores.is_empty()
    }

    /// Returns true if `member` is new.
    pub fn add(&mut self, member: &[u8], score: Score) -> bool {
        match self.scores.insert(member.to_vec(), score) {
            Some(old) => {
                self.order.remove(&(old, member.to_vec())); // must remove the OLD pair
                self.order.insert((score, member.to_vec()));
                false
            }
            None => {
                self.order.insert((score, member.to_vec()));
                true
            }
        }
    }

    /// ZINCRBY. inf + -inf is NaN: refuse instead of corrupting the order.
    pub fn incr(&mut self, member: &[u8], by: f64) -> Result<f64, NanScore> {
        let base = self.scores.get(member).map_or(0.0, |s| s.get());
        let new = Score::new(base + by).ok_or(NanScore)?;
        self.add(member, new);
        Ok(new.get())
    }

    pub fn remove(&mut self, member: &[u8]) -> bool {
        match self.scores.remove(member) {
            Some(score) => {
                self.order.remove(&(score, member.to_vec()));
                true
            }
            None => false,
        }
    }

    pub fn score(&self, member: &[u8]) -> Option<f64> {
        self.scores.get(member).map(|s| s.get())
    }

    /// O(n): counts the entries before this one. Redis' skiplist stores span
    /// counts to make this O(log n); a fine upgrade once everything else works.
    pub fn rank(&self, member: &[u8]) -> Option<usize> {
        let score = *self.scores.get(member)?;
        Some(self.order.range(..(score, member.to_vec())).count())
    }

    /// Members in rank order (`range` comes from `util::index_range`).
    pub fn range(&self, r: std::ops::Range<usize>) -> Vec<(&[u8], f64)> {
        self.order
            .iter()
            .skip(r.start)
            .take(r.len())
            .map(|(s, m)| (m.as_slice(), s.get()))
            .collect()
    }

    #[cfg(test)]
    fn check_invariants(&self) {
        assert_eq!(self.scores.len(), self.order.len());
        for (member, score) in &self.scores {
            assert!(
                self.order.contains(&(*score, member.clone())),
                "order missing {member:?}"
            );
        }
    }
}

/// Score formatting for replies. Rust prints 1.0 as "1" and 1.5 as "1.5",
/// like Redis; infinities need spelling out. Compare exotic values with real Redis.
pub fn format_score(f: f64) -> String {
    if f == f64::INFINITY {
        "inf".into()
    } else if f == f64::NEG_INFINITY {
        "-inf".into()
    } else {
        format!("{f}")
    }
}
```

```rust
#[test]
fn score_rejects_nan_and_orders_totally() {
    assert!(Score::new(f64::NAN).is_none());
    assert_eq!(s(-0.0), s(0.0));
    assert_eq!(s(-0.0).cmp(&s(0.0)), Ordering::Equal);
    assert!(s(f64::NEG_INFINITY) < s(-1e300));
    assert!(s(1e300) < s(f64::INFINITY));
}

#[test]
fn ordering_ties_and_updates() {
    let mut z = ZSet::default();
    assert!(z.add(b"b", s(1.0)));
    assert!(z.add(b"a", s(1.0)));
    assert!(z.add(b"c", s(0.5)));
    let names = |z: &ZSet| {
        z.range(0..z.len())
            .iter()
            .map(|(m, _)| m.to_vec())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&z), vec![b"c".to_vec(), b"a".to_vec(), b"b".to_vec()]);
    assert!(!z.add(b"c", s(9.0)), "update is not 'new'");
    assert_eq!(names(&z), vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
    assert_eq!(z.rank(b"b"), Some(1));
    assert_eq!(z.rank(b"missing"), None);
    assert_eq!(z.incr(b"a", 10.0), Ok(11.0));
    assert_eq!(z.rank(b"a"), Some(2));
    z.check_invariants();
}

#[test]
fn incr_inf_minus_inf_is_refused() {
    let mut z = ZSet::default();
    z.add(b"m", s(f64::INFINITY));
    assert_eq!(z.incr(b"m", f64::NEG_INFINITY), Err(NanScore));
    assert_eq!(
        z.score(b"m"),
        Some(f64::INFINITY),
        "state unchanged on error"
    );
    z.check_invariants();
}

#[test]
fn random_ops_keep_both_indexes_in_agreement() {
    let mut r = Rng(0xDEADBEEFCAFE);
    let mut z = ZSet::default();
    for _ in 0..20_000 {
        let member = [b'm', (r.next() % 30) as u8];
        match r.next() % 4 {
            0 | 1 => {
                z.add(&member, s((r.next() % 10) as f64));
            }
            2 => {
                z.remove(&member);
            }
            _ => {
                let _ = z.incr(&member, (r.next() % 5) as f64 - 2.0);
            }
        }
        z.check_invariants();
    }
}
```

(`Rng` is the 6-line xorshift in the `resp` tests, so no crates are needed.)

---

## 7. Glob matching without recursion

**Where people get stuck.** The textbook recursive matcher is exponential on patterns like `*a*a*a*a*c` against `aaaa...b`, and recursive versions tend to slice or allocate on every call. The standard fix is **single-point backtracking**: remember only the most recent `*` (where the pattern resumes, and which subject position to retry from). On a mismatch, let that star swallow one more byte and retry. That is O(pattern x subject) with no recursion or allocation.

Other decisions to make deliberately: an unterminated `[` matches nothing (no panic, never an out-of-bounds index), a reversed range `[b-a]` is swapped, and `\x` matches `x` literally.

rsedis's `glob_match` (`util/src/lib.rs`) is recursive and calls `.to_vec()` on both slices at each recursion for `*`, and the `[...]` branch indexes `pattern[...]` and `element[elementpos]` directly. From reading it, an unterminated class near the end of the subject appears able to index out of bounds. Treat it as a reference for the *feature list*, not for robustness.

```rust
/// Redis-style glob: `*`, `?`, `[abc]`, `[^abc]`, `[a-z]`, `\x`.
///
/// Iterative with a single backtrack point for the most recent `*`, so it is
/// O(pattern * subject) in the worst case and never recurses or allocates.
/// An unterminated `[` class matches nothing.
pub fn glob_match(pat: &[u8], s: &[u8]) -> bool {
    let (mut p, mut i) = (0, 0);
    // (pattern index just after the last '*', subject index to resume from)
    let mut star: Option<(usize, usize)> = None;

    while i < s.len() {
        if pat.get(p) == Some(&b'*') {
            star = Some((p + 1, i));
            p += 1;
            continue;
        }
        if let Some(next_p) = match_one(pat, p, s[i]) {
            p = next_p;
            i += 1;
            continue;
        }
        match star {
            // Let the star swallow one more byte and retry.
            Some((sp, si)) => {
                p = sp;
                i = si + 1;
                star = Some((sp, si + 1));
            }
            None => return false,
        }
    }
    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

/// If the pattern element at `p` matches the single byte `c`, returns the
/// index of the next pattern element. `None` also when the pattern is exhausted.
fn match_one(pat: &[u8], p: usize, c: u8) -> Option<usize> {
    match *pat.get(p)? {
        b'?' => Some(p + 1),
        b'\\' if p + 1 < pat.len() => (pat[p + 1] == c).then_some(p + 2),
        b'[' => match_class(pat, p + 1, c),
        b => (b == c).then_some(p + 1),
    }
}

fn match_class(pat: &[u8], mut p: usize, c: u8) -> Option<usize> {
    let negate = pat.get(p) == Some(&b'^');
    if negate {
        p += 1;
    }
    let mut matched = false;
    loop {
        match pat.get(p) {
            None => return None, // unterminated class
            Some(b']') => {
                p += 1;
                break;
            }
            Some(b'\\') if p + 1 < pat.len() => {
                matched |= pat[p + 1] == c;
                p += 2;
            }
            Some(&lo)
                if pat.get(p + 1) == Some(&b'-') && p + 2 < pat.len() && pat[p + 2] != b']' =>
            {
                let hi = pat[p + 2];
                let (lo, hi) = if lo > hi { (hi, lo) } else { (lo, hi) };
                matched |= (lo..=hi).contains(&c);
                p += 3;
            }
            Some(&ch) => {
                matched |= ch == c;
                p += 1;
            }
        }
    }
    (matched != negate).then_some(p)
}
```

```rust
#[test]
fn table() {
    let yes: &[(&str, &str)] = &[
        ("*", ""),
        ("*", "anything"),
        ("h?llo", "hello"),
        ("h?llo", "hallo"),
        ("h*llo", "hllo"),
        ("h*llo", "heeeello"),
        ("h[ae]llo", "hello"),
        ("h[^e]llo", "hallo"),
        ("h[a-b]llo", "hbllo"),
        ("h[b-a]llo", "hallo"), // reversed range is swapped
        (r"\*", "*"),
        ("**a**", "xxaxx"),
        ("a*", "a"),
        ("*a*b*", "xxaxxbxx"),
    ];
    for (p, s) in yes {
        assert!(
            glob_match(p.as_bytes(), s.as_bytes()),
            "{p} should match {s}"
        );
    }
    let no: &[(&str, &str)] = &[
        ("h?llo", "hllo"),
        ("h?llo", "heello"),
        ("h[ae]llo", "hillo"),
        ("h[^e]llo", "hello"),
        ("h[a-b]llo", "hcllo"),
        (r"\*", "a"),
        ("[", "a"),
        ("[a", "a"),
        ("", "a"),
        ("a", ""),
        ("*b", "aaa"),
    ];
    for (p, s) in no {
        assert!(
            !glob_match(p.as_bytes(), s.as_bytes()),
            "{p} should not match {s}"
        );
    }
    assert!(glob_match(b"", b""));
}

#[test]
fn no_exponential_blowup() {
    let subject = [vec![b'a'; 5000], vec![b'b']].concat();
    let start = std::time::Instant::now();
    assert!(!glob_match(b"*a*a*a*a*a*a*a*c", &subject));
    assert!(start.elapsed().as_millis() < 500);
}

#[test]
fn malformed_patterns_never_panic() {
    for p in ["[", "[^", "[a-", "[\\", "\\", "[]", "[a-]", "*[", "?[a"] {
        for s in ["", "a", "ab"] {
            let _ = glob_match(p.as_bytes(), s.as_bytes());
        }
    }
}
```

---

## 8. Threaded server and graceful shutdown

**Where people get stuck.** A blocking `listener.accept()` never looks at your stop flag, and a blocking `read` on an idle client never returns, so `join()` hangs forever on Ctrl-C. The pattern:

- Non-blocking listener, poll `accept` and sleep 10 ms on `WouldBlock`.
- A read timeout on each client socket. A timeout is not an error: `Connection` keeps partial frames in its buffer, so the loop just `continue`s and re-checks the flag.
- Sockets accepted from a non-blocking listener may inherit non-blocking mode on some platforms, so `set_nonblocking(false)` explicitly.
- Per-client errors are logged, never propagated. A protocol error closes that connection after sending `-ERR Protocol error`; a command error does not.
- `bind("127.0.0.1:0")` + the handle's `addr` is how tests get a free port.

`session.close(&db)` releases WATCH registrations on every exit path. If you forget, watchers leak for the life of the process.

The background sweeper is a loop of `lock(&db).sweep()` and a sleep. The guard is a temporary, so it is dropped at the end of that statement, before the sleep. If you write `let mut g = lock(&db); g.sweep(); sleep(..)`, you hold the lock while sleeping.

rsedis starts one thread per listener, one detached thread per client (never joined), and a "hz" thread for expiry. Its `Server::stop` (`networking/src/lib.rs`) sends on a channel and then **connects to its own listening address** so the blocking `accept` wakes up and sees the message. That self-connect is the classic alternative to the non-blocking poll used here; it avoids the 10 ms sleep but needs the listener's address and leaves idle client threads running.

```rust
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::command::Command;
use crate::conn::Connection;
use crate::db::{Db, SystemClock};
use crate::exec::{Session, lock};
use crate::resp::Value;

pub struct Server {
    listener: TcpListener,
    db: Arc<Mutex<Db>>,
}

/// Handle to a running server. Dropping it does NOT stop the server; call `shutdown`.
pub struct ServerHandle {
    pub addr: SocketAddr,
    pub db: Arc<Mutex<Db>>,
    stop: Arc<AtomicBool>,
    accept: JoinHandle<Vec<JoinHandle<()>>>,
    sweeper: JoinHandle<()>,
}

impl Server {
    /// Bind to port 0 in tests, then ask `local_addr()` which port you got.
    pub fn bind(addr: &str) -> io::Result<Server> {
        Ok(Server {
            listener: TcpListener::bind(addr)?,
            db: Arc::new(Mutex::new(Db::new(Arc::new(SystemClock)))),
        })
    }

    pub fn start(self) -> io::Result<ServerHandle> {
        let addr = self.listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        // A blocking accept() never looks at `stop`. Poll a non-blocking listener instead.
        self.listener.set_nonblocking(true)?;

        let sweeper = {
            let (db, stop) = (self.db.clone(), stop.clone());
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    lock(&db).sweep(); // lock released at the end of the statement
                    thread::sleep(Duration::from_millis(100));
                }
            })
        };

        let accept = {
            let (db, stop) = (self.db.clone(), stop.clone());
            let listener = self.listener;
            thread::spawn(move || {
                let next_id = AtomicU64::new(1);
                let mut workers = Vec::new();
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _peer)) => {
                            let (db, stop) = (db.clone(), stop.clone());
                            let id = next_id.fetch_add(1, Ordering::Relaxed);
                            workers.push(thread::spawn(move || {
                                if let Err(e) = serve_client(stream, db, stop, id) {
                                    eprintln!("client {id}: {e}"); // never take the server down
                                }
                            }));
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10))
                        }
                        Err(e) => eprintln!("accept: {e}"),
                    }
                }
                workers
            })
        };
        Ok(ServerHandle {
            addr,
            db: self.db,
            stop,
            accept,
            sweeper,
        })
    }
}

impl ServerHandle {
    /// Stop accepting, let each client finish its current command, join everything.
    pub fn shutdown(self) {
        self.stop.store(true, Ordering::Relaxed);
        let workers = self.accept.join().expect("accept thread panicked");
        for w in workers {
            let _ = w.join();
        }
        let _ = self.sweeper.join();
    }
}

fn serve_client(
    stream: TcpStream,
    db: Arc<Mutex<Db>>,
    stop: Arc<AtomicBool>,
    id: u64,
) -> io::Result<()> {
    stream.set_nonblocking(false)?; // accepted sockets may inherit non-blocking on some platforms
    // A read timeout turns "block forever" into "wake up and check the stop flag".
    stream.set_read_timeout(Some(Duration::from_millis(100)))?;
    let mut conn = Connection::new(stream);
    let mut session = Session::new(id);
    while !stop.load(Ordering::Relaxed) {
        let request = match conn.read_value() {
            Ok(Some(v)) => v,
            Ok(None) => break,
            // Timeout: partial data stays in the connection buffer, so just retry.
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                let _ = conn.write_value(&Value::Error(format!("ERR Protocol error: {e}")));
                break; // protocol errors close the connection; command errors do not
            }
            Err(e) => {
                session.close(&db);
                return Err(e);
            }
        };
        // The db lock is taken inside `handle` and released before we touch the socket.
        let reply = session.handle(&db, Command::try_from(request));
        conn.write_value(&reply)?;
    }
    session.close(&db);
    Ok(())
}
```

```rust
fn start() -> ServerHandle {
    Server::bind("127.0.0.1:0").unwrap().start().unwrap()
}

fn client(h: &ServerHandle) -> Connection<TcpStream> {
    let s = TcpStream::connect(h.addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    Connection::new(s)
}

fn call(c: &mut Connection<TcpStream>, args: &[&str]) -> Value {
    let req = Value::Array(Some(args.iter().map(|a| Value::bulk(*a)).collect()));
    c.write_value(&req).unwrap();
    c.read_value().unwrap().expect("reply")
}
```

```rust
#[test]
fn end_to_end_and_error_keeps_connection() {
    let h = start();
    let mut c = client(&h);
    assert_eq!(call(&mut c, &["PING"]), Value::Simple("PONG".into()));
    assert_eq!(call(&mut c, &["set", "k", "v"]), Value::ok());
    assert!(matches!(call(&mut c, &["nosuch"]), Value::Error(_)));
    assert_eq!(
        call(&mut c, &["get", "k"]),
        Value::bulk("v"),
        "connection survived the error"
    );
    h.shutdown();
}

#[test]
fn garbage_closes_only_that_connection() {
    use std::io::{Read, Write};
    let h = start();
    let mut raw = TcpStream::connect(h.addr).unwrap();
    raw.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    raw.write_all(b"garbage\r\n").unwrap();
    let mut out = Vec::new();
    let _ = raw.read_to_end(&mut out);
    assert!(out.starts_with(b"-ERR Protocol error"));
    let mut c = client(&h);
    assert_eq!(call(&mut c, &["PING"]), Value::Simple("PONG".into()));
    h.shutdown();
}

#[test]
fn shutdown_joins_even_with_idle_clients() {
    let h = start();
    let _idle1 = client(&h);
    let _idle2 = client(&h);
    thread::sleep(Duration::from_millis(50));
    let started = Instant::now();
    h.shutdown();
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn active_expiry_runs_in_background() {
    let h = start();
    let mut c = client(&h);
    for i in 0..50 {
        let k = format!("k{i}");
        call(&mut c, &["SET", &k, "v", "PX", "50"]);
    }
    thread::sleep(Duration::from_millis(400));
    // Never read again, yet the sweeper must have reclaimed them.
    assert_eq!(lock(&h.db).len(), 0);
    h.shutdown();
}
```

---

## 9. Pub/Sub with a writer thread

**Where people get stuck.** A subscribed connection must react to two things at once: bytes from its socket and messages published by other clients. With blocking I/O you cannot wait on both. The trick rsedis uses (`create_writer_thread` in `networking/src/lib.rs`) is to give **every** connection one writer thread fed by an `mpsc` channel. Command replies and pub/sub messages are both just values sent into that channel, so the reader thread never needs to select. The registry stores `Sender<Value>` clones.

Details that matter:
- The writer exits when all `Sender`s are gone. The registry holds clones, so on disconnect you must unsubscribe (rsedis loops over `client.subscriptions` and calls `db.unsubscribe`), or the thread lingers.
- `send` fails once the writer is gone. `publish` here uses `retain` to drop dead subscribers as it goes and removes empty channel entries. rsedis only counts successful sends and skips dead ones, so its registry keeps them until explicit unsubscribe.
- The return value of `PUBLISH` is the number of *subscriptions* that received it, so a client matching both a channel and a pattern counts twice. Check against real Redis before relying on that.
- A slow subscriber with an unbounded channel grows memory without limit. Decide whether to bound it (`sync_channel`), drop the subscriber, or accept it, and write the choice down.

In async code this whole design collapses into one `select!` per connection (section 11).

```rust
use std::collections::HashMap;
use std::io::Write;
use std::net::TcpStream;
use std::sync::mpsc::{Sender, channel};
use std::thread::{self, JoinHandle};

use crate::glob::glob_match;
use crate::resp::Value;

/// The trick used by rsedis (`create_writer_thread`): every client owns ONE
/// writer thread fed by an mpsc channel. Command replies AND pub/sub messages
/// are both just `Value`s sent into that channel, so the reader thread never
/// has to wait on "socket or channel" at the same time.
///
/// The thread ends when every `Sender` clone is dropped, i.e. when the reader
/// finished AND the registry pruned/unsubscribed this client.
pub fn spawn_writer(mut stream: TcpStream) -> (Sender<Value>, JoinHandle<()>) {
    let (tx, rx) = channel::<Value>();
    let handle = thread::spawn(move || {
        let mut buf = Vec::new();
        while let Ok(v) = rx.recv() {
            buf.clear();
            v.encode(&mut buf);
            if stream.write_all(&buf).is_err() {
                break; // dropping `rx` makes future sends fail, which prunes us
            }
        }
    });
    (tx, handle)
}

pub type SubscriberId = u64;

#[derive(Default)]
pub struct Registry {
    next_id: SubscriberId,
    channels: HashMap<Vec<u8>, HashMap<SubscriberId, Sender<Value>>>,
    patterns: HashMap<Vec<u8>, HashMap<SubscriberId, Sender<Value>>>,
}

impl Registry {
    fn id(&mut self) -> SubscriberId {
        self.next_id += 1;
        self.next_id
    }

    pub fn subscribe(&mut self, channel: &[u8], tx: Sender<Value>) -> SubscriberId {
        let id = self.id();
        self.channels
            .entry(channel.to_vec())
            .or_default()
            .insert(id, tx);
        id
    }

    pub fn psubscribe(&mut self, pattern: &[u8], tx: Sender<Value>) -> SubscriberId {
        let id = self.id();
        self.patterns
            .entry(pattern.to_vec())
            .or_default()
            .insert(id, tx);
        id
    }

    pub fn unsubscribe(&mut self, channel: &[u8], id: SubscriberId) -> bool {
        remove_from(&mut self.channels, channel, id)
    }

    pub fn punsubscribe(&mut self, pattern: &[u8], id: SubscriberId) -> bool {
        remove_from(&mut self.patterns, pattern, id)
    }

    /// Returns how many subscriptions received the message. A failed `send`
    /// means the client's writer is gone: drop it right here (rsedis only
    /// skips dead senders; pruning avoids leaking them forever).
    pub fn publish(&mut self, channel: &[u8], message: &[u8]) -> usize {
        let mut delivered = 0;
        if let Some(subs) = self.channels.get_mut(channel) {
            let msg = Value::Array(Some(vec![
                Value::bulk("message"),
                Value::bulk(channel),
                Value::bulk(message),
            ]));
            subs.retain(|_, tx| {
                let ok = tx.send(msg.clone()).is_ok();
                delivered += ok as usize;
                ok
            });
            if subs.is_empty() {
                self.channels.remove(channel); // no empty entries left behind
            }
        }
        for (pattern, subs) in self.patterns.iter_mut() {
            if !glob_match(pattern, channel) {
                continue;
            }
            let msg = Value::Array(Some(vec![
                Value::bulk("pmessage"),
                Value::bulk(pattern.as_slice()),
                Value::bulk(channel),
                Value::bulk(message),
            ]));
            subs.retain(|_, tx| {
                let ok = tx.send(msg.clone()).is_ok();
                delivered += ok as usize;
                ok
            });
        }
        self.patterns.retain(|_, subs| !subs.is_empty());
        delivered
    }
}

fn remove_from(
    map: &mut HashMap<Vec<u8>, HashMap<SubscriberId, Sender<Value>>>,
    name: &[u8],
    id: SubscriberId,
) -> bool {
    let Some(subs) = map.get_mut(name) else {
        return false;
    };
    let removed = subs.remove(&id).is_some();
    if subs.is_empty() {
        map.remove(name);
    }
    removed
}
```

```rust
#[test]
fn delivers_counts_and_prunes_dead_receivers() {
    let mut reg = Registry::default();
    let (tx1, rx1) = channel();
    let (tx2, rx2) = channel();
    reg.subscribe(b"news", tx1);
    reg.subscribe(b"news", tx2);
    assert_eq!(reg.publish(b"news", b"hi"), 2);
    assert_eq!(rx1.try_recv().unwrap(), msg("news", "hi"));
    assert_eq!(rx2.try_recv().unwrap(), msg("news", "hi"));
    drop(rx2); // client disconnected
    assert_eq!(reg.publish(b"news", b"again"), 1);
    drop(rx1);
    assert_eq!(reg.publish(b"news", b"x"), 0);
    assert!(reg.channels.is_empty(), "no empty entries kept");
    assert_eq!(reg.publish(b"other", b"x"), 0);
}

#[test]
fn message_reaches_a_real_socket_via_the_writer_thread() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let (server_side, _) = listener.accept().unwrap();

    let (tx, writer) = spawn_writer(server_side);
    let mut reg = Registry::default();
    let id = reg.subscribe(b"news", tx.clone());
    tx.send(Value::ok()).unwrap(); // a normal reply goes through the same pipe
    assert_eq!(reg.publish(b"news", b"hello"), 1);

    let mut conn = Connection::new(client);
    assert_eq!(conn.read_value().unwrap(), Some(Value::ok()));
    assert_eq!(conn.read_value().unwrap(), Some(msg("news", "hello")));

    // Disconnect cleanup: unsubscribe + drop our sender => writer thread exits.
    reg.unsubscribe(b"news", id);
    drop(tx);
    writer.join().unwrap();
}
```

(The `msg(channel, payload)` helper in the tests builds the `["message", channel, payload]` array.)

---

## 10. `MULTI`/`EXEC`/`WATCH`

**Where people get stuck.** Three things:

1. **State machine.** Model the connection as an enum (`Normal` / `Queuing { queue, aborted }`) and `match` on `(state, request)`. The nested-`MULTI`, `EXEC`-without-`MULTI` and `WATCH`-inside-`MULTI` errors fall out as separate arms instead of flags scattered around.
2. **Atomicity.** `EXEC` takes the db lock **once** and runs every queued command plus the WATCH check under it. This only works because `execute` is pure (section 5).
3. **WATCH.** Redis flags watchers "dirty" when a watched key is written; `EXEC` refuses if the flag is set. `Watchers` does that with a `key -> clients` map and a `dirty` set; every write path in `Db` calls `touch`. Note the subtleties: a *delete* and an *expiry* count as modifications too, a dirty flag survives until `EXEC`/`DISCARD`/`UNWATCH`, and `EXEC`/`DISCARD`/disconnect must all release the watches.

Behaviour to match: an error while *queuing* (unknown command, wrong arity) marks the transaction aborted and `EXEC` answers `-EXECABORT`. A runtime error inside `EXEC` (say `INCR` on a string) does **not** roll anything back; that element of the reply array is an error and the rest apply.

rsedis (`command/src/command.rs`): `multi` sets `client.multi = true`, queued commands go into `client.multi_commands`, `exec` swaps the vector out and runs each with `command(c.get_command(), db, client).unwrap()`, which would panic if a queued command ever returned a "wait" instead of a reply. `watch` registers with `db.key_watch(...)`; `key_updated` drops the watchers for a key; `exec` returns `Nil` when `key_watch_verify` finds a watcher missing. It has no `EXECABORT` handling.

The `Session` code is in the `exec.rs` listing in section 5; the `Watchers` struct is in `db.rs` in section 4. Tests:

```rust
fn cmd(args: &[&str]) -> Result<Command, CommandError> {
    let owned: Vec<Vec<u8>> = args.iter().map(|a| a.as_bytes().to_vec()).collect();
    Command::from_args(&owned)
}

fn shared() -> Mutex<Db> {
    Mutex::new(Db::new(ManualClock::new()))
}

fn run(s: &mut Session, db: &Mutex<Db>, args: &[&str]) -> Value {
    s.handle(db, cmd(args))
}

fn arr(items: Vec<Value>) -> Value {
    Value::Array(Some(items))
}
```

```rust
#[test]
fn watch_aborts_when_key_changed() {
    let db = shared();
    let (mut a, mut b) = (Session::new(1), Session::new(2));
    run(&mut a, &db, &["watch", "k"]);
    run(&mut b, &db, &["set", "k", "x"]);
    run(&mut a, &db, &["multi"]);
    run(&mut a, &db, &["set", "k", "mine"]);
    assert_eq!(run(&mut a, &db, &["exec"]), Value::Array(None));
    assert_eq!(run(&mut a, &db, &["get", "k"]), Value::bulk("x"));
    // Watches were cleared by EXEC: next transaction goes through.
    run(&mut a, &db, &["multi"]);
    run(&mut a, &db, &["set", "k", "mine"]);
    assert_eq!(run(&mut a, &db, &["exec"]), arr(vec![Value::ok()]));
}

#[test]
fn runtime_error_inside_exec_does_not_roll_back() {
    let db = shared();
    let mut s = Session::new(1);
    run(&mut s, &db, &["set", "s", "abc"]);
    run(&mut s, &db, &["multi"]);
    run(&mut s, &db, &["incr", "s"]);
    run(&mut s, &db, &["set", "b", "1"]);
    let Value::Array(Some(r)) = run(&mut s, &db, &["exec"]) else {
        panic!()
    };
    assert!(matches!(r[0], Value::Error(_)));
    assert_eq!(r[1], Value::ok());
}

#[test]
fn nested_multi_and_queuing_errors() {
    let db = shared();
    let mut s = Session::new(1);
    run(&mut s, &db, &["multi"]);
    assert!(matches!(run(&mut s, &db, &["multi"]), Value::Error(e) if e.contains("nested")));
    assert!(matches!(run(&mut s, &db, &["nosuch"]), Value::Error(_)));
    run(&mut s, &db, &["set", "a", "1"]);
    assert!(
        matches!(run(&mut s, &db, &["exec"]), Value::Error(e) if e.starts_with("EXECABORT"))
    );
    assert_eq!(
        run(&mut s, &db, &["get", "a"]),
        Value::nil(),
        "nothing applied"
    );
}

#[test]
fn exec_is_atomic_across_threads() {
    let db = Arc::new(Mutex::new(Db::new(Arc::new(SystemClock))));
    let writer = {
        let db = db.clone();
        std::thread::spawn(move || {
            let mut s = Session::new(1);
            for _ in 0..2000 {
                s.handle(&db, cmd(&["multi"]));
                s.handle(&db, cmd(&["incr", "x"]));
                s.handle(&db, cmd(&["incr", "x"]));
                s.handle(&db, cmd(&["exec"]));
            }
        })
    };
    let mut reader = Session::new(2);
    for _ in 0..2000 {
        if let Value::Bulk(Some(v)) = reader.handle(&db, cmd(&["get", "x"])) {
            let n: i64 = String::from_utf8(v).unwrap().parse().unwrap();
            assert_eq!(n % 2, 0, "observed a half-applied transaction");
        }
    }
    writer.join().unwrap();
}
```

---

## 11. Async: the same parser, `BLPOP`, shutdown

`Cargo.toml` additions:

```toml
[dependencies]
bytes = "1"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time", "net", "io-util"] }
```

**(a) The parser did not change.** `BytesMut` derefs to `[u8]`, so `parse(&self.buf)` works as is; after a success, `advance(used)` drops the consumed bytes (cheaper than `Vec::drain`). `read_buf` appends straight into the `BytesMut`. This is the payoff of writing `parse` over `&[u8]` in Task 5.

**(b) `read_value` is cancel safe**, which is what permits it inside `select!`. All progress lives in `self.buf`, and `read_buf` is documented as cancel safe. If you instead kept a half-parsed frame in a local variable, a `select!` that picked another branch would silently lose it.

**(c) Blocking `BLPOP` with `Notify`.** Rules that make it correct:
- Create the `Notified` future **before** checking the list. `notify_waiters` wakes every `Notified` created before the call, even one never polled yet, so a push between "check" and "await" cannot be lost. (`notify_waiters_reaches_futures_created_earlier_but_not_later_ones` below pins that behaviour down. `Notified::enable()` is not required for this.)
- The pop happens **synchronously inside `try_pop` under the lock, with no `.await` between removing the element and returning it.** If the future is dropped (client disconnect, timeout, shutdown), either the pop already happened and the value is returned, or it never happened. No element is lost.
- After waking, re-check; another waiter may have won. Waking everyone is simple and correct but gives up Redis' FIFO fairness between blocked clients. A fair version keeps a per-key queue of `oneshot` senders.
- `std::sync::Mutex` (not `tokio::sync::Mutex`) is right for `Db`: critical sections are short and never await. The compiler enforces it: holding a `MutexGuard` across `.await` makes the future `!Send` and `tokio::spawn` will refuse it.
- `BLPOP` is intercepted in the connection loop because it must `.await`; `Session::handle` stays synchronous.

rsedis has no async, and its blocking commands (`generic_bpop` in `command/src/command.rs`) work differently: a command returns `Err(ResponseError::Wait(receiver))`; the client thread blocks on that receiver; the command spawned one thread that sleeps for the timeout and then sends `None` (the client replies nil), and another that waits for a key-updated notification (`db.key_subscribe` / `key_updated`) and then constructs a brand-new `BLPOP` command with the remaining timeout to retry. The author's own comment on that block is "This code is ugly. I was stuck for a week...". It spends a thread per blocked client and per timeout; the `Notify` version spends none.

**(d) Shutdown** is a `watch<bool>` plus `select!` in the accept loop and in every connection, and a `JoinSet` to wait for the connections to finish. A client blocked in `BLPOP` is also cancellable by the same signal.

```rust
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::{Buf, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, watch};
use tokio::task::JoinSet;
use tokio::time::{Instant, timeout_at};

use crate::command::{Command, CommandError, into_args};
use crate::db::{Db, WrongType};
use crate::exec::{Session, lock};
use crate::resp::{ParseError, Value, parse};

// ---- Connection: the SAME parser, now over a BytesMut ----------------------

pub struct AsyncConnection<S> {
    stream: S,
    buf: BytesMut,
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncConnection<S> {
    pub fn new(stream: S) -> Self {
        AsyncConnection {
            stream,
            buf: BytesMut::with_capacity(4096),
        }
    }

    /// Cancel safe: all progress lives in `self.buf`, and `read_buf` itself is
    /// cancel safe. That is what makes it legal inside `select!`.
    pub async fn read_value(&mut self) -> io::Result<Option<Value>> {
        loop {
            match parse(&self.buf) {
                // BytesMut derefs to [u8], so `parse` needed no changes.
                Ok((value, used)) => {
                    self.buf.advance(used);
                    return Ok(Some(value));
                }
                Err(ParseError::Incomplete) => {}
                Err(ParseError::Invalid(m)) => {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, m));
                }
            }
            if self.stream.read_buf(&mut self.buf).await? == 0 {
                return if self.buf.is_empty() {
                    Ok(None)
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "peer closed mid-frame",
                    ))
                };
            }
        }
    }

    pub async fn write_value(&mut self, v: &Value) -> io::Result<()> {
        let mut out = Vec::new();
        v.encode(&mut out);
        self.stream.write_all(&out).await
    }
}

// ---- Shared state + blocking list pop -----------------------------------------

pub struct Shared {
    /// std Mutex on purpose: critical sections are short and never `.await`.
    pub db: std::sync::Mutex<Db>,
    /// Pulsed after writes so blocked BLPOPs re-check their lists.
    pub list_pushed: Notify,
}

impl Shared {
    pub fn new(db: Db) -> Arc<Shared> {
        Arc::new(Shared {
            db: std::sync::Mutex::new(db),
            list_pushed: Notify::new(),
        })
    }
}

/// Synchronous: pops under the lock and returns. There is NO `.await` between
/// removing the element and handing it to the caller, so dropping (cancelling)
/// the BLPOP future can never lose an element.
/// (key, value) popped from the first non-empty list.
pub type Popped = (Vec<u8>, Vec<u8>);

fn try_pop(shared: &Shared, keys: &[Vec<u8>]) -> Result<Option<Popped>, WrongType> {
    let mut db = lock(&shared.db);
    for key in keys {
        if let Some(Some(v)) = db.with_list(key, false, |l| l.pop_front())? {
            return Ok(Some((key.clone(), v)));
        }
    }
    Ok(None)
}

/// `timeout: None` blocks forever (BLPOP ... 0).
pub async fn blpop(
    shared: &Shared,
    keys: &[Vec<u8>],
    timeout: Option<Duration>,
) -> Result<Option<Popped>, WrongType> {
    let deadline = timeout.map(|t| Instant::now() + t);
    loop {
        // 1. Create the Notified future BEFORE checking. `notify_waiters` wakes
        //    every Notified created before the call, even one never polled yet,
        //    so a push landing between our check and our await is not lost.
        //    (Creating it after the check would reintroduce that race.)
        let notified = shared.list_pushed.notified();

        // 2. Check.
        if let Some(hit) = try_pop(shared, keys)? {
            return Ok(Some(hit));
        }

        // 3. Wait for a push, or give up at the deadline.
        match deadline {
            None => notified.await,
            Some(d) => {
                if timeout_at(d, notified).await.is_err() {
                    return Ok(None);
                }
            }
        }
        // Woken: someone pushed *somewhere*. Loop and re-check; if another
        // waiter beat us to the element, we simply wait again.
    }
}

// ---- Server ----------------------------------------------------------------------

/// Runs until `shutdown` flips to true, then drains in-flight connections.
pub async fn run(listener: TcpListener, shared: Arc<Shared>, mut shutdown: watch::Receiver<bool>) {
    let next_id = AtomicU64::new(1);
    let mut conns = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let id = next_id.fetch_add(1, Ordering::Relaxed);
                    let (shared, shutdown) = (shared.clone(), shutdown.clone());
                    conns.spawn(async move {
                        if let Err(e) = handle_connection(stream, shared, shutdown, id).await {
                            eprintln!("client {id}: {e}");
                        }
                    });
                }
                Err(e) => eprintln!("accept: {e}"),
            },
            _ = shutdown.changed() => break,
        }
    }
    while conns.join_next().await.is_some() {}
}

async fn handle_connection(
    stream: TcpStream,
    shared: Arc<Shared>,
    mut shutdown: watch::Receiver<bool>,
    id: u64,
) -> io::Result<()> {
    let mut conn = AsyncConnection::new(stream);
    let mut session = Session::new(id);
    let result = loop {
        let request = tokio::select! {
            r = conn.read_value() => match r {
                Ok(Some(v)) => v,
                Ok(None) => break Ok(()),
                Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                    let _ = conn.write_value(&Value::Error(format!("ERR Protocol error: {e}"))).await;
                    break Ok(());
                }
                Err(e) => break Err(e),
            },
            _ = shutdown.changed() => break Ok(()),
        };
        // BLPOP is intercepted here because it must `.await`; Session::handle is sync.
        let reply = match into_args(request) {
            Ok(args)
                if args
                    .first()
                    .is_some_and(|n| n.eq_ignore_ascii_case(b"BLPOP")) =>
            {
                // A blocked client can still be cancelled by shutdown.
                tokio::select! {
                    r = blpop_command(&shared, &args[1..]) => r,
                    _ = shutdown.changed() => break Ok(()),
                }
            }
            Ok(args) => {
                let reply = session.handle(&shared.db, Command::from_args(&args));
                // Cheap when nobody waits. Narrow it to list pushes once it matters.
                shared.list_pushed.notify_waiters();
                reply
            }
            Err(e) => e.into(),
        };
        if let Err(e) = conn.write_value(&reply).await {
            break Err(e);
        }
    };
    session.close(&shared.db);
    result
}

async fn blpop_command(shared: &Shared, args: &[Vec<u8>]) -> Value {
    let Some((timeout_arg, keys)) = args.split_last().filter(|(_, keys)| !keys.is_empty()) else {
        return CommandError::WrongArity("blpop".into()).into();
    };
    let secs = std::str::from_utf8(timeout_arg)
        .ok()
        .and_then(|s| s.parse::<f64>().ok());
    let timeout = match secs {
        Some(0.0) => None,
        Some(s) if s > 0.0 => match Duration::try_from_secs_f64(s) {
            Ok(d) => Some(d),
            Err(_) => return Value::Error("ERR timeout is out of range".into()),
        },
        _ => return Value::Error("ERR timeout is not a float or out of range".into()),
    };
    match blpop(shared, keys, timeout).await {
        Ok(Some((k, v))) => Value::Array(Some(vec![Value::bulk(k), Value::bulk(v)])),
        Ok(None) => Value::Array(None),
        Err(WrongType) => {
            Value::Error("WRONGTYPE Operation against a key holding the wrong kind of value".into())
        }
    }
}
```

```rust
fn shared() -> Arc<Shared> {
    Shared::new(Db::new(Arc::new(SystemClock)))
}

fn push(shared: &Shared, key: &str, val: &str) {
    let cmd = Command::LPush(key.as_bytes().to_vec(), vec![val.as_bytes().to_vec()]);
    crate::exec::execute(&mut lock(&shared.db), cmd);
    shared.list_pushed.notify_waiters();
}

fn k(s: &str) -> Vec<Vec<u8>> {
    vec![s.as_bytes().to_vec()]
}
```

```rust
#[tokio::test]
async fn notify_waiters_reaches_futures_created_earlier_but_not_later_ones() {
    let n = Notify::new();
    let before = n.notified(); // created, never polled
    n.notify_waiters();
    assert!(timeout(Duration::from_millis(100), before).await.is_ok());
    n.notify_waiters();
    let after = n.notified();
    assert!(
        timeout(Duration::from_millis(100), after).await.is_err(),
        "a late Notified misses it"
    );
}

#[tokio::test]
async fn cancelling_a_blocked_pop_loses_nothing() {
    let s = shared();
    // Client gives up after 30ms (like a disconnect inside select!).
    assert!(
        timeout(Duration::from_millis(30), blpop(&s, &k("q"), None))
            .await
            .is_err()
    );
    push(&s, "q", "x");
    assert_eq!(
        try_pop(&s, &k("q")).unwrap(),
        Some((b"q".to_vec(), b"x".to_vec()))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_lost_wakeups_under_races() {
    let s = shared();
    for i in 0..500 {
        let waiter = tokio::spawn({
            let s = s.clone();
            async move { blpop(&s, &k("q"), None).await }
        });
        // No sleep: the push races the waiter's check/await window.
        push(&s, "q", &i.to_string());
        let got = timeout(Duration::from_secs(2), waiter)
            .await
            .expect("lost wakeup")
            .unwrap()
            .unwrap();
        assert_eq!(got.unwrap().1, i.to_string().into_bytes());
    }
}

#[tokio::test]
async fn end_to_end_blpop_between_clients() {
    let (addr, stop, server) = start().await;
    let mut a = AsyncConnection::new(TcpStream::connect(addr).await.unwrap());
    let mut b = AsyncConnection::new(TcpStream::connect(addr).await.unwrap());
    assert_eq!(call(&mut a, &["PING"]).await, Value::Simple("PONG".into()));

    let blocked = tokio::spawn(async move { call(&mut a, &["BLPOP", "q", "0"]).await });
    sleep(Duration::from_millis(50)).await;
    assert_eq!(call(&mut b, &["LPUSH", "q", "x"]).await, Value::Integer(1));
    assert_eq!(
        blocked.await.unwrap(),
        Value::Array(Some(vec![Value::bulk("q"), Value::bulk("x")]))
    );

    assert_eq!(
        call(&mut b, &["BLPOP", "q", "0.1"]).await,
        Value::Array(None),
        "timeout -> nil array"
    );
    assert!(matches!(
        call(&mut b, &["BLPOP", "q"]).await,
        Value::Error(_)
    ));
    assert!(matches!(
        call(&mut b, &["BLPOP", "q", "abc"]).await,
        Value::Error(_)
    ));

    stop.send(true).unwrap();
    timeout(Duration::from_secs(2), server)
        .await
        .expect("shutdown hung")
        .unwrap();
}

#[tokio::test]
async fn shutdown_with_idle_and_blocked_clients() {
    let (addr, stop, server) = start().await;
    let mut idle = Vec::new();
    for _ in 0..20 {
        idle.push(AsyncConnection::new(
            TcpStream::connect(addr).await.unwrap(),
        ));
    }
    let mut blocked = AsyncConnection::new(TcpStream::connect(addr).await.unwrap());
    blocked
        .write_value(&Value::Array(Some(vec![
            Value::bulk("BLPOP"),
            Value::bulk("q"),
            Value::bulk("0"),
        ])))
        .await
        .unwrap();
    sleep(Duration::from_millis(50)).await;
    let started = StdInstant::now();
    stop.send(true).unwrap();
    timeout(Duration::from_secs(2), server)
        .await
        .expect("shutdown hung")
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
}
```

The helpers `start()` and `call()` used by the last two tests spawn `run` on `127.0.0.1:0` and send one command, respectively; they are about ten lines each.

Limit of the race test: `no_lost_wakeups_under_races` is a smoke test. It also passes if you create the `Notified` after the check, because the window is tiny. The deterministic test above is the one that pins the ordering rule down.

---

## 12. Snapshots that survive corruption

**Where people get stuck.** (1) Allocating from an untrusted length (`vec![0; len]` on a corrupt file asks for 4 GB). (2) Writing the format by hand and then discovering you cannot tell a good file from a damaged one. (3) Using `Instant` for persisted expiry.

What the implementation does:
- **Never allocate from a length prefix.** `read.take(len).read_to_end(&mut v)` grows the buffer only as bytes actually arrive; a 20-byte file claiming 400 MB fails with `UnexpectedEof` immediately. Counts are not used for `with_capacity` either. A hard `MAX_BLOB` rejects absurd values up front.
- **CRC32 as a `Write`/`Read` wrapper** (`CrcWriter`, `CrcReader`). The format code stays oblivious to checksumming. The trailing checksum is written to the *inner* writer so it is not part of its own input. CRC32 detects every single-bit error, and the test flips every bit of a sample file.
- **Format**: magic, version byte, tagged records, an end tag, then the checksum. A tag-terminated stream (instead of an up-front record count) lets you write it without knowing the size first.
- **Generic over `Write`/`Read`**, so tests use `Vec<u8>` and `&[u8]`. Production writes to a temp file and `rename`s it over the old snapshot so a crash mid-save never destroys the previous one.
- **Wall-clock time on disk.** `capture` converts each `Instant` deadline to unix milliseconds using a `now_unix_ms` you pass in; `restore` converts back and skips keys whose deadline already passed while the server was down.
- **BGSAVE without `fork`.** `capture` clones the data while you hold the lock; write the file *after* releasing it.

rsedis does not implement `SAVE`/`BGSAVE` (they are unchecked in its `TODO.md`). It does have per-value serialization for `DUMP` in `database/rdbutil` (`encode_len`, `encode_slice_u8`), which is Redis' own RDB encoding and a reasonable thing to read if you want your format to be Redis-compatible.

```rust
use std::io::{self, Read, Write};
use std::time::Duration;

use crate::db::{Data, Db};

const MAGIC: &[u8; 4] = b"RRDB";
const VERSION: u8 = 1;
const MAX_BLOB: u64 = 512 * 1024 * 1024;

const TAG_END: u8 = 0;
const TAG_STR: u8 = 1;
const TAG_LIST: u8 = 2;

#[derive(Debug, Clone, PartialEq)]
pub struct SnapEntry {
    pub key: Vec<u8>,
    pub data: Data,
    /// Absolute wall-clock time. `Instant` is meaningless after a restart.
    pub expire_at_unix_ms: Option<u64>,
}

// ---- CRC32 as a Write/Read wrapper -------------------------------------

fn crc32_update(mut crc: u32, bytes: &[u8]) -> u32 {
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    crc
}

struct CrcWriter<W> {
    inner: W,
    crc: u32,
}
impl<W: Write> Write for CrcWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.crc = crc32_update(self.crc, &buf[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

struct CrcReader<R> {
    inner: R,
    crc: u32,
}
impl<R: Read> Read for CrcReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.crc = crc32_update(self.crc, &buf[..n]);
        Ok(n)
    }
}

// ---- encoding ------------------------------------------------------------

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

fn write_blob<W: Write>(w: &mut W, b: &[u8]) -> io::Result<()> {
    let len = u32::try_from(b.len()).map_err(|_| bad("blob too large"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(b)
}

fn read_u32<R: Read>(r: &mut R) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

/// The important trick: NEVER `vec![0; len]` from an untrusted length.
/// `take(len).read_to_end` only allocates as bytes actually arrive, so a
/// corrupt "4 GB" prefix on a 20-byte file fails fast with UnexpectedEof.
fn read_blob<R: Read>(r: &mut R) -> io::Result<Vec<u8>> {
    let len = read_u32(r)? as u64;
    if len > MAX_BLOB {
        return Err(bad("blob length exceeds limit"));
    }
    let mut v = Vec::new();
    r.by_ref().take(len).read_to_end(&mut v)?;
    if v.len() as u64 != len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "truncated blob",
        ));
    }
    Ok(v)
}

/// Generic over `Write`: tests use a `Vec<u8>`, production uses a `File`.
pub fn write_snapshot<W: Write>(w: W, entries: &[SnapEntry]) -> io::Result<()> {
    let mut w = CrcWriter {
        inner: w,
        crc: 0xFFFF_FFFF,
    };
    w.write_all(MAGIC)?;
    w.write_all(&[VERSION])?;
    for e in entries {
        match &e.data {
            Data::Str(_) => w.write_all(&[TAG_STR])?,
            Data::List(_) => w.write_all(&[TAG_LIST])?,
        }
        let exp = e.expire_at_unix_ms.map_or(-1i64, |t| t as i64);
        w.write_all(&exp.to_le_bytes())?;
        write_blob(&mut w, &e.key)?;
        match &e.data {
            Data::Str(s) => write_blob(&mut w, s)?,
            Data::List(l) => {
                w.write_all(
                    &u32::try_from(l.len())
                        .map_err(|_| bad("list too long"))?
                        .to_le_bytes(),
                )?;
                for item in l {
                    write_blob(&mut w, item)?;
                }
            }
        }
    }
    w.write_all(&[TAG_END])?;
    let crc = !w.crc;
    w.inner.write_all(&crc.to_le_bytes())?; // the checksum itself is not checksummed
    w.inner.flush()
}

pub fn read_snapshot<R: Read>(r: R) -> io::Result<Vec<SnapEntry>> {
    let mut r = CrcReader {
        inner: r,
        crc: 0xFFFF_FFFF,
    };
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(bad("not a snapshot file"));
    }
    let mut version = [0u8; 1];
    r.read_exact(&mut version)?;
    if version[0] != VERSION {
        return Err(bad("unsupported snapshot version"));
    }
    let mut entries = Vec::new(); // no with_capacity: we don't trust any count
    loop {
        let mut tag = [0u8; 1];
        r.read_exact(&mut tag)?;
        if tag[0] == TAG_END {
            break;
        }
        let mut exp = [0u8; 8];
        r.read_exact(&mut exp)?;
        let exp = i64::from_le_bytes(exp);
        let key = read_blob(&mut r)?;
        let data = match tag[0] {
            TAG_STR => Data::Str(read_blob(&mut r)?),
            TAG_LIST => {
                let n = read_u32(&mut r)?;
                let mut list = std::collections::VecDeque::new();
                for _ in 0..n {
                    list.push_back(read_blob(&mut r)?);
                }
                Data::List(list)
            }
            _ => return Err(bad("unknown value tag")),
        };
        entries.push(SnapEntry {
            key,
            data,
            expire_at_unix_ms: u64::try_from(exp).ok(),
        });
    }
    let expected = !r.crc;
    let stored = read_u32(&mut r.inner)?;
    if stored != expected {
        return Err(bad("checksum mismatch"));
    }
    Ok(entries)
}

// ---- Db <-> entries --------------------------------------------------------

/// Clone the data while holding the lock (fast), then write the file after
/// releasing it (slow). That is BGSAVE without `fork`.
pub fn capture(db: &Db, now_unix_ms: u64) -> Vec<SnapEntry> {
    let now = db.now();
    db.entries()
        .filter_map(|(key, e)| {
            let expire_at_unix_ms = match e.expires_at {
                None => None,
                Some(t) if t <= now => return None, // already expired, don't persist
                Some(t) => Some(now_unix_ms + t.duration_since(now).as_millis() as u64),
            };
            Some(SnapEntry {
                key: key.clone(),
                data: e.data.clone(),
                expire_at_unix_ms,
            })
        })
        .collect()
}

/// Entries whose wall-clock deadline passed while the server was down are skipped.
pub fn restore(db: &mut Db, entries: Vec<SnapEntry>, now_unix_ms: u64) {
    let now = db.now();
    for e in entries {
        let expires_at = match e.expire_at_unix_ms {
            None => None,
            Some(at) if at <= now_unix_ms => continue,
            Some(at) => Some(now + Duration::from_millis(at - now_unix_ms)),
        };
        db.load_entry(e.key, e.data, expires_at);
    }
}
```

```rust
fn sample() -> Vec<SnapEntry> {
    vec![
        SnapEntry {
            key: b"s".to_vec(),
            data: Data::Str(b"hello".to_vec()),
            expire_at_unix_ms: None,
        },
        SnapEntry {
            key: vec![0, 255, 13, 10],
            data: Data::Str(vec![]),
            expire_at_unix_ms: Some(5_000),
        },
        SnapEntry {
            key: b"l".to_vec(),
            data: Data::List(VecDeque::from([b"a".to_vec(), vec![], vec![1, 2, 3]])),
            expire_at_unix_ms: None,
        },
    ]
}

fn dump(entries: &[SnapEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    write_snapshot(&mut out, entries).unwrap();
    out
}
```

```rust
#[test]
fn every_single_bit_flip_is_detected() {
    let bytes = dump(&sample());
    for i in 0..bytes.len() {
        for bit in 0..8 {
            let mut b = bytes.clone();
            b[i] ^= 1 << bit;
            assert!(read_snapshot(&b[..]).is_err(), "flip byte {i} bit {bit}");
        }
    }
}

#[test]
fn absurd_lengths_fail_fast_without_allocating() {
    let mut b = Vec::from(&b"RRDB"[..]);
    b.extend([VERSION, TAG_STR]);
    b.extend((-1i64).to_le_bytes());
    for claimed in [u32::MAX, 400_000_000] {
        let mut c = b.clone();
        c.extend(claimed.to_le_bytes());
        c.extend(b"abc");
        let started = std::time::Instant::now();
        assert!(read_snapshot(&c[..]).is_err());
        assert!(started.elapsed().as_millis() < 200);
    }
}

#[test]
fn db_round_trip_preserves_ttl_and_skips_keys_that_expired_while_down() {
    let clock = ManualClock::new();
    let mut db = Db::new(clock.clone());
    db.set(b"keep", b"1".to_vec(), false, false, None);
    db.set(
        b"short",
        b"2".to_vec(),
        false,
        false,
        Some(Duration::from_secs(1)),
    );
    db.set(
        b"long",
        b"3".to_vec(),
        false,
        false,
        Some(Duration::from_secs(60)),
    );
    db.with_list(b"l", true, |l| l.push_back(b"x".to_vec()))
        .unwrap();

    let wall = 1_000_000;
    let bytes = dump(&capture(&db, wall));

    // "Restart" 10 seconds later.
    let clock2 = ManualClock::new();
    let mut db2 = Db::new(clock2.clone());
    restore(&mut db2, read_snapshot(&bytes[..]).unwrap(), wall + 10_000);
    assert_eq!(db2.get_str(b"keep"), Ok(Some(&b"1"[..])));
    assert_eq!(db2.get_str(b"short"), Ok(None), "expired while down");
    assert_eq!(db2.pttl(b"long"), 50_000);
    assert_eq!(db2.len(), 3);
}
```

---

## 13. AOF: torn writes and deterministic logging

**Where people get stuck.** (1) Logging the raw command: `EXPIRE k 10` replayed a week later extends the key's life, and `SPOP` removes a different member. (2) Logging commands that failed or did nothing. (3) Replay appending to the log it is reading. (4) A crash mid-write leaves half a record, and the loader must tell that apart from real corruption.

- **`log_form` runs after execution** and looks at the reply: errors and no-op NX/XX produce nothing, `SET ... EX 10` becomes `SET` plus `PEXPIREAT <absolute ms>`, `PEXPIRE` becomes `PEXPIREAT`. To finish the loop, add a `PEXPIREAT` command that converts unix ms to a deadline, as in `snapshot::restore`; `SPOP`-style random commands should log what they actually removed (`SREM`).
- **One `write_all` per record** keeps a record contiguous, so a crash leaves at most one torn record at the end.
- **`replay` distinguishes** `Incomplete` at the end (torn tail) from `Invalid` anywhere (corruption). The reuse of `parse` is why Task 5's `Incomplete`/`Invalid` split matters again here.
- **Truncation is opt-in** (`allow_truncated_tail`). A corrupt *length prefix* in the middle of the file also parses as `Incomplete` (it waits for bytes that never come), so truncating unconditionally would silently discard everything after the damage. Redis and rsedis both put this behind an `aof-load-truncated` option and refuse to start otherwise.
- **Replay must not log.** Open the `Aof` for appending only after `replay` returns, and run replayed commands through the same `execute` but with no logging hook.
- Reading the whole file into memory is the simple version. Stream it through the Task 6 `Connection` for large files.

rsedis (`persistence/src/aof.rs`, `command/src/aof.rs`): `Aof::write` appends the request's *raw bytes* (`command.get_data()`), emitting a `SELECT n` record first whenever the target database changes. `load` runs each parsed command through the normal `command::command` with `db.loading = true`, then at end of file, if bytes remain unparsed, either exits with an "Unexpected end of file reading the append only file" message or (with `aof_load_truncated`) calls `aof.truncate(parser.position)`. That is the same torn-tail logic as above; it does not rewrite expirations, which is the main thing `log_form` adds.

```rust
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::command::{Command, into_args};
use crate::resp::{ParseError, Value, parse};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fsync {
    Always,
    EverySec,
    No,
}

pub struct Aof {
    file: File,
    policy: Fsync,
    last_sync: Instant,
}

impl Aof {
    pub fn open(path: &Path, policy: Fsync) -> io::Result<Aof> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Aof {
            file,
            policy,
            last_sync: Instant::now(),
        })
    }

    /// Appends one command as a RESP array of bulk strings: the same format
    /// clients send, so replay can reuse `parse`. (rsedis writes the raw request bytes.)
    pub fn append(&mut self, args: &[Vec<u8>]) -> io::Result<()> {
        let record = Value::Array(Some(args.iter().map(|a| Value::bulk(a.clone())).collect()));
        let mut buf = Vec::new();
        record.encode(&mut buf);
        self.file.write_all(&buf)?; // one write_all per record keeps records contiguous
        match self.policy {
            Fsync::Always => self.file.sync_data()?,
            Fsync::EverySec if self.last_sync.elapsed() >= Duration::from_secs(1) => {
                self.file.sync_data()?;
                self.last_sync = Instant::now();
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct ReplayStats {
    pub applied: usize,
    /// Bytes of a half-written final record that were cut off.
    pub truncated_bytes: usize,
}

/// Feeds every complete record to `apply`. Replay must NOT append to the log
/// again: open the `Aof` for writing only after this returns.
///
/// - Incomplete bytes at the end: usually a torn write (crash mid-record).
///   With `allow_truncated_tail`, cut the file back to the last good record
///   and carry on; without it, refuse to start. The flag exists because a
///   corrupt *length prefix* in the middle of the file also looks incomplete,
///   and truncating there would silently throw away everything after it.
///   (Redis and rsedis both gate this behind `aof-load-truncated`.)
/// - Invalid bytes anywhere: real corruption. Refuse to start.
pub fn replay(
    path: &Path,
    allow_truncated_tail: bool,
    mut apply: impl FnMut(Vec<Vec<u8>>),
) -> io::Result<ReplayStats> {
    let data = match fs::read(path) {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(ReplayStats::default()),
        Err(e) => return Err(e),
    };
    let mut stats = ReplayStats::default();
    let mut pos = 0;
    while pos < data.len() {
        match parse(&data[pos..]) {
            Ok((value, used)) => {
                let args = into_args(value).map_err(|_| corrupt(pos, "record is not a command"))?;
                apply(args);
                stats.applied += 1;
                pos += used;
            }
            Err(ParseError::Incomplete) if !allow_truncated_tail => {
                return Err(corrupt(pos, "incomplete record at end of file"));
            }
            Err(ParseError::Incomplete) => {
                stats.truncated_bytes = data.len() - pos;
                OpenOptions::new()
                    .write(true)
                    .open(path)?
                    .set_len(pos as u64)?;
                break;
            }
            Err(ParseError::Invalid(msg)) => return Err(corrupt(pos, &msg)),
        }
    }
    Ok(stats)
}

fn corrupt(offset: usize, why: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("corrupt AOF at byte {offset}: {why}"),
    )
}

/// Turns an executed command into what must be logged: only successful writes,
/// and only deterministic forms. "Expire in 10s" replayed next week would be
/// wrong, so it becomes "expire at <absolute unix ms>".
pub fn log_form(cmd: &Command, reply: &Value, now_unix_ms: u64) -> Vec<Vec<Vec<u8>>> {
    let b = |s: &str| s.as_bytes().to_vec();
    if matches!(reply, Value::Error(_)) {
        return vec![];
    }
    match cmd {
        // NX/XX that did not apply replied nil: nothing happened, log nothing.
        Command::Set { .. } if *reply == Value::nil() => vec![],
        Command::Set {
            key, value, ttl, ..
        } => {
            let mut out = vec![vec![b("SET"), key.clone(), value.clone()]];
            if let Some(t) = ttl {
                let at = now_unix_ms + t.as_millis() as u64;
                out.push(vec![
                    b("PEXPIREAT"),
                    key.clone(),
                    at.to_string().into_bytes(),
                ]);
            }
            out
        }
        Command::Del(keys) if *reply != Value::Integer(0) => {
            vec![
                std::iter::once(b("DEL"))
                    .chain(keys.iter().cloned())
                    .collect(),
            ]
        }
        Command::IncrBy(k, d) => vec![vec![b("INCRBY"), k.clone(), d.to_string().into_bytes()]],
        Command::LPush(k, vals) => vec![[vec![b("LPUSH"), k.clone()], vals.clone()].concat()],
        Command::LPop(k) if *reply != Value::nil() => vec![vec![b("LPOP"), k.clone()]],
        Command::PExpire(k, ms) if *reply == Value::Integer(1) => {
            let at = now_unix_ms + ms;
            vec![vec![b("PEXPIREAT"), k.clone(), at.to_string().into_bytes()]]
        }
        _ => vec![], // reads, and writes that changed nothing
    }
}
```

```rust
fn tmp(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("rrr-aof-{}-{name}", std::process::id()));
    let _ = fs::remove_file(&p);
    p
}

fn args(a: &[&str]) -> Vec<Vec<u8>> {
    a.iter().map(|s| s.as_bytes().to_vec()).collect()
}

fn write_log(path: &Path, cmds: &[&[&str]]) {
    let mut aof = Aof::open(path, Fsync::No).unwrap();
    for c in cmds {
        aof.append(&args(c)).unwrap();
    }
}

type Replayed = io::Result<(Vec<Vec<Vec<u8>>>, ReplayStats)>;

fn collect_with(path: &Path, allow_truncated_tail: bool) -> Replayed {
    let mut got = vec![];
    let stats = replay(path, allow_truncated_tail, |a| got.push(a))?;
    Ok((got, stats))
}

fn collect(path: &Path) -> Replayed {
    collect_with(path, true)
}
```

```rust
#[test]
fn torn_tail_at_every_offset_recovers() {
    let p = tmp("torn");
    write_log(
        &p,
        &[&["SET", "a", "1"], &["SET", "b", "2"], &["SET", "c", "3"]],
    );
    let full = fs::read(&p).unwrap();
    let (first_two, _) = collect(&p).unwrap();
    let two_len: usize = {
        let mut tmp = Vec::new();
        for a in &first_two[..2] {
            Value::Array(Some(a.iter().map(|x| Value::bulk(x.clone())).collect()))
                .encode(&mut tmp);
        }
        tmp.len()
    };
    for cut in two_len + 1..full.len() {
        fs::write(&p, &full[..cut]).unwrap();
        let (got, stats) = collect(&p).unwrap();
        assert_eq!(got.len(), 2, "cut at {cut}");
        assert_eq!(stats.truncated_bytes, cut - two_len);
        assert_eq!(
            fs::metadata(&p).unwrap().len() as usize,
            two_len,
            "file truncated to last good record"
        );
        // Appending after recovery yields a clean log again.
        write_log(&p, &[&["SET", "d", "4"]]);
        assert_eq!(collect(&p).unwrap().0.len(), 3);
    }
    let _ = fs::remove_file(&p);
}

#[test]
fn torn_tail_is_refused_unless_explicitly_allowed() {
    let p = tmp("strict");
    write_log(&p, &[&["SET", "a", "1"], &["SET", "b", "2"]]);
    let full = fs::read(&p).unwrap();
    fs::write(&p, &full[..full.len() - 3]).unwrap();
    let err = collect_with(&p, false).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        fs::read(&p).unwrap().len(),
        full.len() - 3,
        "file left untouched"
    );
    assert_eq!(collect_with(&p, true).unwrap().0.len(), 1);
    let _ = fs::remove_file(&p);
}

#[test]
fn mid_file_corruption_is_an_error_not_a_silent_truncation() {
    let p = tmp("corrupt");
    write_log(&p, &[&["SET", "a", "1"], &["SET", "b", "2"]]);
    let mut data = fs::read(&p).unwrap();
    data[0] = b'?'; // destroy the first record's type byte
    fs::write(&p, &data).unwrap();
    let err = collect(&p).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("byte 0"));
    assert_eq!(fs::read(&p).unwrap(), data, "file untouched");
    let _ = fs::remove_file(&p);
}

#[test]
fn log_form_rewrites_non_determinism_and_skips_noops() {
    let set_ex = Command::Set {
        key: b"k".to_vec(),
        value: b"v".to_vec(),
        nx: false,
        xx: false,
        ttl: Some(Duration::from_secs(10)),
    };
    assert_eq!(
        log_form(&set_ex, &Value::ok(), 1_000),
        vec![args(&["SET", "k", "v"]), args(&["PEXPIREAT", "k", "11000"])]
    );
    let set_nx = Command::Set {
        key: b"k".to_vec(),
        value: b"v".to_vec(),
        nx: true,
        xx: false,
        ttl: None,
    };
    assert!(
        log_form(&set_nx, &Value::nil(), 0).is_empty(),
        "NX that did nothing"
    );
    assert!(
        log_form(&Command::Get(b"k".to_vec()), &Value::nil(), 0).is_empty(),
        "reads are not logged"
    );
    assert!(
        log_form(
            &Command::IncrBy(b"k".to_vec(), 1),
            &Value::Error("ERR x".into()),
            0
        )
        .is_empty(),
        "failed commands are not logged"
    );
    assert_eq!(
        log_form(
            &Command::PExpire(b"k".to_vec(), 500),
            &Value::Integer(1),
            1_000
        ),
        vec![args(&["PEXPIREAT", "k", "1500"])]
    );
    assert!(
        log_form(
            &Command::PExpire(b"k".to_vec(), 500),
            &Value::Integer(0),
            1_000
        )
        .is_empty()
    );
    assert!(log_form(&Command::Del(vec![b"k".to_vec()]), &Value::Integer(0), 0).is_empty());
}
```

---

## 14. Trap checklist

Quick scan before you declare a task done. Each item is something that went wrong, or nearly did, while building the code above.

- [ ] A parser that returns one error type cannot support fragmented reads. Split `Incomplete` from `Invalid`.
- [ ] Any length that comes off the wire is hostile: cap it, and don't `with_capacity` or `vec![0; n]` from it.
- [ ] `str::parse::<i64>` accepts `+1`; Redis doesn't. Compare with real Redis.
- [ ] `E0502` around `map.get` + `self.remove`? Compute a `bool` or clone the small thing first so the borrow ends.
- [ ] Two indexes over the same data (`deadlines` + `map`, `order` + `scores`): all mutation through one or two private methods, plus a test that checks the invariant after random operations.
- [ ] `-0.0 == 0.0` but `total_cmp` orders them. Normalise before using a float in an `Ord`.
- [ ] Don't hold a lock across socket I/O or `.await`. Scope it with a block or a temporary.
- [ ] A blocking `accept`/`read` ignores your stop flag. Poll with non-blocking or timeouts.
- [ ] Every connection exit path must release per-connection registrations (watchers, subscriptions).
- [ ] Create the `Notified` future before checking the condition; do the state change synchronously with no `.await` between it and returning.
- [ ] Log what happened, not what was asked (AOF): after execution, deterministic, no-ops omitted.
- [ ] Never truncate a file you cannot prove is only torn at the tail.
- [ ] Persist wall-clock time, never `Instant`.
- [ ] Prefer a test that fails when the property breaks. Mutate your own code (delete the line you believe matters) and see whether a test notices.
