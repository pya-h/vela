use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::thread;

fn main() {
    let listener = Arc::new(TcpListener::bind("127.0.0.1:18260").expect("bind failed"));
    let mut handles = Vec::with_capacity(4);

    for _ in 0..4 {
        let l = Arc::clone(&listener);
        handles.push(thread::spawn(move || {
            let mut buf = [0u8; 2048];
            for stream in l.incoming() {
                if let Ok(mut s) = stream {
                    let mut total = 0;
                    loop {
                        match s.read(&mut buf[total..]) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                total += n;
                                if buf[..total].windows(4).any(|w| w == b"\r\n\r\n")
                                    || total >= buf.len()
                                {
                                    break;
                                }
                            }
                        }
                    }
                    let req = &buf[..total];
                    if req.starts_with(b"GET /json ") {
                        let _ = s.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 26\r\nConnection: close\r\n\r\n{\"status\":\"ok\",\"value\":42}",
                        );
                    } else if req.starts_with(b"GET /health ") {
                        let _ = s.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                        );
                    } else {
                        let _ = s.write_all(
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nConnection: close\r\n\r\nnot found",
                        );
                    }
                }
            }
        }));
    }

    for h in handles {
        let _ = h.join();
    }
}
