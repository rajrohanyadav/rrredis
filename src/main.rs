use std::io;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

//struct Server {}

fn handle(mut stream: TcpStream) -> io::Result<()> {
    let mut buffer = [0u8; 1024];
    loop {
        let n = stream.read(&mut buffer)?;
        if n == 0 {
            return Ok(());
        }
        stream.write_all(&buffer[..n])?;
    }
}

fn main() -> std::io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:6380")?;
    println!("Server listening on port 6380...");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(e) = handle(stream) {
                    eprintln!("connection error: {e}");
                }
            }
            Err(e) => eprintln!("connection failed: {}", e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn spawn_server() -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let _ = handle(stream.unwrap());
            }
        });
        addr
    }

    #[test]
    fn echoes_exactly_what_was_sent() {
        let addr = spawn_server();
        let mut client = TcpStream::connect(addr).unwrap();

        client.write_all(b"hello\n").unwrap();

        let mut got = [0u8; 6];
        client.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"hello\n");
    }

    #[test]
    fn echoes_binary_bytes_unchanged() {
        let addr = spawn_server();
        let mut client = TcpStream::connect(addr).unwrap();

        let payload: Vec<u8> = (0..=255).collect();
        client.write_all(&payload).unwrap();

        let mut got = vec![0u8; payload.len()];
        client.read_exact(&mut got).unwrap();
        assert_eq!(got, payload);
    }

    #[test]
    fn multiple_messages_on_one_connection() {
        let addr = spawn_server();
        let mut client = TcpStream::connect(addr).unwrap();

        for msg in [&b"one\n"[..], b"two\n", b"three\n"] {
            client.write_all(msg).unwrap();
            let mut got = vec![0u8; msg.len()];
            client.read_exact(&mut got).unwrap();
            assert_eq!(got, msg);
        }
    }

    #[test]
    fn server_accepts_next_client_after_one_disconnects() {
        let addr = spawn_server();

        drop(TcpStream::connect(addr).unwrap()); // connect, then close at once

        let mut second = TcpStream::connect(addr).unwrap();
        second.write_all(b"still alive\n").unwrap();
        let mut got = [0u8; 12];
        second.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"still alive\n");
    }

    use std::net::Shutdown; // add this to the test module's imports

    #[test]
    fn closing_write_side_ends_session_after_echo() {
        let addr = spawn_server();
        let mut client = TcpStream::connect(addr).unwrap();

        client.write_all(b"bye").unwrap();
        client.shutdown(Shutdown::Write).unwrap(); // half-close: "I'm done sending"

        let mut got = Vec::new();
        client.read_to_end(&mut got).unwrap(); // reads until the server closes
        assert_eq!(got, b"bye");
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
    fn echoes_one_megabyte() {
        let addr = spawn_server();
        let mut writer = TcpStream::connect(addr).unwrap();
        let mut reader = writer.try_clone().unwrap();

        let payload: Vec<u8> = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
        let to_send = payload.clone();

        let t = thread::spawn(move || {
            writer.write_all(&to_send).unwrap();
            writer.shutdown(Shutdown::Write).unwrap();
        });

        let mut got = Vec::new();
        reader.read_to_end(&mut got).unwrap();
        t.join().unwrap();

        assert_eq!(got.len(), payload.len());
        assert!(got == payload);
    }
}
