//! A tiny HTTP server for tests.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::thread;

/// Serves each `(path, body)` with 200 OK, and 404 for anything else, until
/// the test process exits. Returns the base URL.
pub fn serve(routes: Vec<(String, Vec<u8>)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a local port");
    let base = format!("http://{}", listener.local_addr().expect("local address"));
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            let mut reader = BufReader::new(&stream);
            let mut request = String::new();
            let _ = reader.read_line(&mut request);
            // Drain the headers.
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|read| read > 2) {
                line.clear();
            }
            let path = request.split_whitespace().nth(1).unwrap_or("/");
            let (status, body) = routes
                .iter()
                .find(|(route, _)| route == path)
                .map_or(("404 Not Found", &[][..]), |(_, body)| {
                    ("200 OK", body.as_slice())
                });
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(body);
        }
    });
    base
}
