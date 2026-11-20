use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::time::Duration;
use std::{io, thread};

const MAX_LINE: u64 = 1024 * 1024;
const DRAIN_LIMIT: u64 = 64 * 1024;
const ERR_LINE_TOO_LONG: &[u8] = b"-ERR line too long\r\n";
const ERR_UNKNOWN_COMMAND: &[u8] = b"-ERR unknown command\r\n";
const ERR_INVALID_UTF_8: &[u8] = b"-ERR invalid utf-8\r\n";

fn reply(line: &str) -> Vec<u8> {
    let line = line.trim();
    let mut parts = line.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim_start();

    if cmd.eq_ignore_ascii_case("PING") {
        b"+PONG\r\n".to_vec()
    } else if cmd.eq_ignore_ascii_case("ECHO") {
        let mut out = Vec::with_capacity(rest.len() + 2);
        out.extend_from_slice(rest.as_bytes());
        out.extend_from_slice(b"\r\n");
        out
    } else {
        ERR_UNKNOWN_COMMAND.to_vec()
    }
}

fn handle(stream: TcpStream) -> io::Result<()> {
    let mut reader = BufReader::new(&stream);
    let mut writer = &stream;

    let mut line = String::new();
    loop {
        line.clear();
        let n = match (&mut reader).take(MAX_LINE).read_line(&mut line) {
            Ok(n) => {
                if n as u64 == MAX_LINE && !line.ends_with('\n') {
                    writer.write_all(ERR_LINE_TOO_LONG)?;
                    stream.shutdown(Shutdown::Write)?;
                    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
                    let _ = io::copy(&mut (&mut reader).take(DRAIN_LIMIT), &mut io::sink());
                    return Ok(());
                }
                n
            }
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                writer.write_all(ERR_INVALID_UTF_8)?;
                continue;
            }
            Err(e) => return Err(e),
        };
        if n == 0 {
            return Ok(());
        }
        writer.write_all(&reply(&line))?;
    }
}

fn serve(listener: TcpListener) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let peer = stream
                    .peer_addr()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|_| "unknown".to_string());
                eprintln!("connected {peer}");
                thread::spawn(move || match handle(stream) {
                    Ok(()) => eprintln!("disconnected {peer}"),
                    Err(e) => eprintln!("connection error {peer}: {e}"),
                });
            }
            Err(e) => eprintln!("connection failed {e}"),
        }
    }
}

fn main() -> io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:6380")?;
    println!("Server listening on port 6380...");
    serve(listener);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    fn spawn_server() -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || serve(listener));
        addr
    }

    #[test]
    fn handle_returns_ok_on_immediate_close() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        drop(TcpStream::connect(addr).unwrap()); // connect, then close at once
        let (stream, _) = listener.accept().unwrap();

        assert!(handle(stream).is_ok());
    }

    #[test]
    fn server_accepts_next_client_after_one_disconnects() {
        let addr = spawn_server();

        drop(TcpStream::connect(addr).unwrap()); // connect, then close at once

        let mut second = TcpStream::connect(addr).unwrap();
        second.write_all(b"echo still alive\n").unwrap();
        let mut got = [0u8; 13];
        second.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"still alive\r\n");
    }

    #[test]
    fn closing_write_side_ends_session_after_echo() {
        let addr = spawn_server();
        let mut client = TcpStream::connect(addr).unwrap();

        client.write_all(b"echo bye").unwrap();
        client.shutdown(Shutdown::Write).unwrap(); // half-close: "I'm done sending"

        let mut got = Vec::new();
        client.read_to_end(&mut got).unwrap(); // reads until the server closes
        assert_eq!(got, b"bye\r\n");
    }

    #[test]
    fn twenty_clients_each_get_their_own_echo() {
        const N: usize = 20;
        let addr = spawn_server();
        let barrier = Arc::new(Barrier::new(N));

        let handles: Vec<_> = (0..N)
            .map(|i| {
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let mut c = TcpStream::connect(addr).unwrap();
                    c.set_read_timeout(Some(Duration::from_secs(2))).unwrap();

                    barrier.wait();

                    let msg = format!("echo client-{i}\n");
                    let expected = format!("client-{i}\r\n");
                    c.write_all(msg.as_bytes()).unwrap();
                    let mut got = vec![0u8; expected.len()];
                    c.read_exact(&mut got).unwrap();
                    assert_eq!(&got, expected.as_bytes());
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn server_survives_client_that_drops_with_unread_data() {
        let addr = spawn_server();

        {
            let mut rude = TcpStream::connect(addr).unwrap();
            rude.write_all(&[7u8; 64 * 1024]).unwrap();
        }

        thread::sleep(Duration::from_millis(100));

        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        c.write_all(b"echo alive\n").unwrap();
        let mut got = [0u8; 7];
        c.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"alive\r\n");
    }

    #[test]
    fn reply_table() {
        let cases: &[(&str, &[u8])] = &[
            ("PING", b"+PONG\r\n"),
            ("ping", b"+PONG\r\n"),
            ("PiNg", b"+PONG\r\n"),
            ("PING\r\n", b"+PONG\r\n"),
            ("  PING  ", b"+PONG\r\n"),
            ("ECHO hi", b"hi\r\n"),
            ("echo hello world", b"hello world\r\n"),
            ("echo    padded", b"padded\r\n"),
            ("ECHO", b"\r\n"),
            ("FOO", ERR_UNKNOWN_COMMAND),
            ("", ERR_UNKNOWN_COMMAND),
            ("\r\n", ERR_UNKNOWN_COMMAND),
        ];

        for (input, expected) in cases {
            assert_eq!(&reply(input), expected, "input {input:?}");
        }
    }

    #[test]
    fn oversized_line_gets_an_error_then_closes_connection() {
        let addr = spawn_server();
        let mut client = TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let input = vec![b'a'; MAX_LINE as usize + 1];
        client.write_all(&input).unwrap();
        let mut got = vec![];

        client.read_to_end(&mut got).unwrap();
        assert_eq!(got.as_slice(), ERR_LINE_TOO_LONG);
    }

    #[test]
    fn line_exactly_at_the_limit_is_accepted() {
        let addr = spawn_server();
        let mut client = TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();

        let payload = vec![b'a'; MAX_LINE as usize - "echo ".len() - 1];
        let mut request = b"echo ".to_vec();
        request.extend_from_slice(&payload);
        request.push(b'\n');
        assert_eq!(request.len() as u64, MAX_LINE);
        client.write_all(&request).unwrap();

        let mut expected = payload;
        expected.extend_from_slice(b"\r\n");
        let mut got = vec![0u8; expected.len()];
        client.read_exact(&mut got).unwrap();
        assert!(got == expected);
    }

    #[test]
    fn line_one_byte_over_the_limit_is_rejected() {
        let addr = spawn_server();
        let mut client = TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();

        let mut payload = b"echo ".to_vec();
        payload.extend_from_slice(&vec![b'a'; MAX_LINE as usize - "echo ".len()]);
        payload.push(b'\n');
        assert_eq!(payload.len() as u64, MAX_LINE + 1);
        client.write_all(&payload).unwrap();

        let mut got = Vec::new();
        client.read_to_end(&mut got).unwrap();
        assert_eq!(got.as_slice(), ERR_LINE_TOO_LONG);
    }

    #[test]
    fn cap_applies_per_line_not_per_connection() {
        let addr = spawn_server();
        let mut client = TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();

        let payload = "x".repeat(10_000);
        let request = format!("echo {payload}\n");
        let expected = format!("{payload}\r\n");

        for _ in 0..200 {
            client.write_all(request.as_bytes()).unwrap();
            let mut got = vec![0u8; expected.len()];
            client.read_exact(&mut got).unwrap();
            assert_eq!(got, expected.as_bytes());
        }
    }

    #[test]
    fn invalid_utf8_gets_an_error_and_connection_survives() {
        let addr = spawn_server();
        let mut client = TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();

        client.write_all(b"\xff\xfe\n").unwrap();
        client.write_all(b"PING\n").unwrap();

        let mut expected = ERR_INVALID_UTF_8.to_vec();
        expected.extend_from_slice(b"+PONG\r\n");
        let mut got = vec![0u8; expected.len()];
        client.read_exact(&mut got).unwrap();
        assert_eq!(got.as_slice(), expected);
    }
}
