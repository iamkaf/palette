//! A client for Minecraft's remote console (RCON), which sends commands to a running server.

use crate::Result;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

const LOGIN: i32 = 3;
const COMMAND: i32 = 2;

pub struct Rcon {
    stream: TcpStream,
    next_id: i32,
}

impl Rcon {
    pub fn connect(port: u16, password: &str) -> Result<Self> {
        let stream = TcpStream::connect(("127.0.0.1", port))?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        let mut rcon = Self { stream, next_id: 1 };
        let id = rcon.send(LOGIN, password)?;
        let (reply_id, _) = rcon.receive()?;
        // The server answers a wrong password with request ID -1.
        if reply_id != id {
            return Err("the server rejected the RCON password".into());
        }
        Ok(rcon)
    }

    /// Runs a command and returns what the server replied.
    pub fn command(&mut self, command: &str) -> Result<String> {
        self.send(COMMAND, command)?;
        Ok(self.receive()?.1)
    }

    fn send(&mut self, kind: i32, payload: &str) -> Result<i32> {
        let id = self.next_id;
        self.next_id += 1;
        let body_length = 4 + 4 + payload.len() + 2;
        let mut packet = Vec::with_capacity(4 + body_length);
        packet.extend_from_slice(
            &i32::try_from(body_length)
                .map_err(|_| "RCON command too long")?
                .to_le_bytes(),
        );
        packet.extend_from_slice(&id.to_le_bytes());
        packet.extend_from_slice(&kind.to_le_bytes());
        packet.extend_from_slice(payload.as_bytes());
        packet.extend_from_slice(&[0, 0]);
        self.stream.write_all(&packet)?;
        Ok(id)
    }

    fn receive(&mut self) -> Result<(i32, String)> {
        let mut length = [0; 4];
        self.stream.read_exact(&mut length)?;
        let length = usize::try_from(i32::from_le_bytes(length))
            .map_err(|_| "invalid RCON packet length")?;
        if !(10..=64 * 1024).contains(&length) {
            return Err(format!("invalid RCON packet length {length}").into());
        }
        let mut body = vec![0; length];
        self.stream.read_exact(&mut body)?;
        let id = i32::from_le_bytes([body[0], body[1], body[2], body[3]]);
        let payload = String::from_utf8_lossy(&body[8..length - 2]).into_owned();
        Ok((id, payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    fn read_packet(stream: &mut TcpStream) -> (i32, i32, String) {
        let mut length = [0; 4];
        stream.read_exact(&mut length).expect("length");
        let mut body = vec![0; i32::from_le_bytes(length) as usize];
        stream.read_exact(&mut body).expect("body");
        let id = i32::from_le_bytes(body[0..4].try_into().expect("id"));
        let kind = i32::from_le_bytes(body[4..8].try_into().expect("kind"));
        (
            id,
            kind,
            String::from_utf8_lossy(&body[8..body.len() - 2]).into_owned(),
        )
    }

    fn write_packet(stream: &mut TcpStream, id: i32, payload: &str) {
        let mut packet = Vec::new();
        packet.extend_from_slice(&((10 + payload.len()) as i32).to_le_bytes());
        packet.extend_from_slice(&id.to_le_bytes());
        packet.extend_from_slice(&0i32.to_le_bytes());
        packet.extend_from_slice(payload.as_bytes());
        packet.extend_from_slice(&[0, 0]);
        stream.write_all(&packet).expect("write");
    }

    #[test]
    fn logs_in_and_runs_a_command() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listen");
        let port = listener.local_addr().expect("address").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let (id, kind, password) = read_packet(&mut stream);
            assert_eq!((kind, password.as_str()), (LOGIN, "secret"));
            write_packet(&mut stream, id, "");
            let (id, kind, command) = read_packet(&mut stream);
            assert_eq!((kind, command.as_str()), (COMMAND, "reload"));
            write_packet(&mut stream, id, "Reloading!");
        });

        let mut rcon = Rcon::connect(port, "secret").expect("login");
        assert_eq!(rcon.command("reload").expect("command"), "Reloading!");
        server.join().expect("server");
    }

    #[test]
    fn a_wrong_password_is_rejected() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listen");
        let port = listener.local_addr().expect("address").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            read_packet(&mut stream);
            write_packet(&mut stream, -1, "");
        });
        assert!(Rcon::connect(port, "wrong").is_err());
        server.join().expect("server");
    }
}
