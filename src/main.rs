use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::{io, thread};

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
    use std::{
        net::Shutdown,
        sync::{Arc, Barrier},
        time::Duration,
    };

    fn spawn_server() -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || serve(listener));
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

    #[test]
    fn two_clients_at_once() {
        let addr = spawn_server();

        let mut client1 = TcpStream::connect(addr).unwrap();
        let mut client2 = TcpStream::connect(addr).unwrap();

        client2
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client2.write_all(b"hello2\n").unwrap();
        let mut got2 = [0u8; 7];
        client2.read_exact(&mut got2).unwrap();
        assert_eq!(&got2, b"hello2\n");

        client1
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client1.write_all(b"hello1\n").unwrap();
        let mut got1 = [0u8; 7];
        client1.read_exact(&mut got1).unwrap();
        assert_eq!(&got1, b"hello1\n");
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

                    let msg = format!("client-{i}\n");
                    c.write_all(msg.as_bytes()).unwrap();
                    let mut got = vec![0u8; msg.len()];
                    c.read_exact(&mut got).unwrap();
                    assert_eq!(&got, msg.as_bytes());
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
        c.write_all(b"alive\n").unwrap();
        let mut got = [0u8; 6];
        c.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"alive\n");
    }
}
