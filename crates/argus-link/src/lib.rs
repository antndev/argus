use std::io::ErrorKind;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use tungstenite::client::IntoClientRequest;
use tungstenite::handshake::HandshakeError;
use tungstenite::http::header::AUTHORIZATION;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Error, Message, WebSocket};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(250);

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

#[derive(Default)]
pub struct Link {
    socket: Option<Socket>,
    failures: u32,
    retry: Option<Instant>,
    poked: bool,
}

pub fn socket_url(url: &str) -> String {
    match url.split_once("://") {
        Some(("https", rest)) => format!("wss://{rest}"),
        Some(("http", rest)) => format!("ws://{rest}"),
        _ => url.to_string(),
    }
}

impl Link {
    pub fn failures(&self) -> u32 {
        self.failures
    }

    pub fn connected(&self) -> bool {
        self.socket.is_some()
    }

    pub fn close(&mut self) {
        if let Some(mut socket) = self.socket.take() {
            let _ = socket.close(None);
            let _ = socket.flush();
        }
        self.failures = 0;
        self.retry = None;
        self.poked = false;
    }

    pub fn request(&mut self, url: &str, token: &str, body: Vec<u8>, timeout: Duration) -> Result<Vec<u8>, String> {
        if self.socket.is_none() {
            if self.retry.is_some_and(|at| Instant::now() < at) {
                return Err("waiting to reconnect".to_string());
            }
            match connect(url, token) {
                Ok(socket) => self.socket = Some(socket),
                Err(error) => return Err(self.fail(error)),
            }
        }
        match self.exchange(body, timeout) {
            Ok(reply) => {
                self.failures = 0;
                self.retry = None;
                Ok(reply)
            }
            Err(error) => Err(self.fail(error)),
        }
    }

    pub fn wait(&mut self, timeout: Duration) -> Result<bool, String> {
        if std::mem::take(&mut self.poked) {
            return Ok(true);
        }
        let Some(socket) = self.socket.as_mut() else { return Err("not connected".to_string()) };
        let deadline = Instant::now() + timeout;
        let outcome = loop {
            match socket.read() {
                Ok(Message::Text(_)) => break Ok(true),
                Ok(Message::Close(_)) => break Err("closed by the node".to_string()),
                Ok(_) => {}
                Err(error) if idle(&error) => {
                    if Instant::now() >= deadline {
                        break Ok(false);
                    }
                }
                Err(error) => break Err(error.to_string()),
            }
        };
        outcome.map_err(|error| self.fail(error))
    }

    fn exchange(&mut self, body: Vec<u8>, timeout: Duration) -> Result<Vec<u8>, String> {
        let socket = self.socket.as_mut().ok_or("not connected")?;
        socket.send(Message::Binary(body.into())).map_err(|e| e.to_string())?;
        let deadline = Instant::now() + timeout;
        loop {
            match socket.read() {
                Ok(Message::Binary(reply)) => return Ok(reply.to_vec()),
                Ok(Message::Text(_)) => self.poked = true,
                Ok(Message::Close(frame)) => return Err(frame.map_or_else(|| "closed by the node".to_string(), |f| f.reason.to_string())),
                Ok(_) => {}
                Err(error) if idle(&error) => {
                    if Instant::now() >= deadline {
                        return Err("no reply in time".to_string());
                    }
                }
                Err(error) => return Err(error.to_string()),
            }
        }
    }

    fn fail(&mut self, error: String) -> String {
        self.socket = None;
        self.poked = false;
        self.failures += 1;
        self.retry = Some(Instant::now() + Duration::from_secs(1 << self.failures.min(5)));
        error
    }
}

fn idle(error: &Error) -> bool {
    matches!(error, Error::Io(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut))
}

fn tcp(socket: &Socket) -> Option<&TcpStream> {
    match socket.get_ref() {
        MaybeTlsStream::Plain(stream) => Some(stream),
        MaybeTlsStream::Rustls(stream) => Some(stream.get_ref()),
        _ => None,
    }
}

fn connect(url: &str, token: &str) -> Result<Socket, String> {
    let mut request = socket_url(url).into_client_request().map_err(|e| e.to_string())?;
    let header = format!("Bearer {token}").parse().map_err(|_| "the token cannot be sent as a header")?;
    request.headers_mut().insert(AUTHORIZATION, header);
    let uri = request.uri();
    let host = uri.host().ok_or("the url has no host")?.trim_start_matches('[').trim_end_matches(']').to_string();
    let port = uri.port_u16().unwrap_or(if uri.scheme_str() == Some("wss") { 443 } else { 80 });
    let stream = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .find_map(|address| TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).ok())
        .ok_or_else(|| format!("cannot connect to {host}:{port}"))?;
    stream.set_read_timeout(Some(CONNECT_TIMEOUT)).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT)).map_err(|e| e.to_string())?;
    let _ = stream.set_nodelay(true);
    let socket = match tungstenite::client_tls(request, stream) {
        Ok((socket, _)) => socket,
        Err(HandshakeError::Failure(error)) => return Err(error.to_string()),
        Err(HandshakeError::Interrupted(_)) => return Err("the handshake timed out".to_string()),
    };
    tcp(&socket).ok_or("unsupported stream")?.set_read_timeout(Some(POLL)).map_err(|e| e.to_string())?;
    Ok(socket)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;
    use tungstenite::handshake::server::{ErrorResponse, Request, Response};

    #[allow(clippy::result_large_err)]
    fn server(sessions: usize, token: &'static str) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}/api/v1/ingest", listener.local_addr().expect("address"));
        let handle = thread::spawn(move || {
            for stream in listener.incoming().take(sessions) {
                let check = |request: &Request, response: Response| {
                    let sent = request.headers().get(AUTHORIZATION).and_then(|v| v.to_str().ok());
                    if sent == Some(&format!("Bearer {token}")) { Ok(response) } else { Err(ErrorResponse::new(Some("no".into()))) }
                };
                let Ok(mut socket) = tungstenite::accept_hdr(stream.expect("stream"), check) else { continue };
                if let Ok(Message::Binary(body)) = socket.read() {
                    let _ = socket.send(Message::Text("changed".into()));
                    let mut reply = body.to_vec();
                    reply.reverse();
                    let _ = socket.send(Message::Binary(reply.into()));
                }
                let _ = socket.close(None);
                while socket.read().is_ok() {}
            }
        });
        (url, handle)
    }

    #[test]
    fn urls_become_socket_urls() {
        assert_eq!(socket_url("https://node.example.com/api/v1/ingest"), "wss://node.example.com/api/v1/ingest");
        assert_eq!(socket_url("http://127.0.0.1:8090/api/v1/sync"), "ws://127.0.0.1:8090/api/v1/sync");
    }

    #[test]
    fn requests_get_replies_and_reconnect_after_a_close() {
        let (url, handle) = server(2, "secret");
        let mut link = Link::default();
        assert_eq!(link.request(&url, "secret", vec![1, 2, 3], Duration::from_secs(5)), Ok(vec![3, 2, 1]));
        assert_eq!(link.wait(Duration::from_millis(10)), Ok(true));
        assert!(link.wait(Duration::from_secs(5)).is_err());
        assert_eq!(link.failures(), 1);
        assert_eq!(link.request(&url, "secret", vec![9], Duration::from_secs(5)), Err("waiting to reconnect".to_string()));
        link.retry = None;
        assert_eq!(link.request(&url, "secret", vec![4, 5], Duration::from_secs(5)), Ok(vec![5, 4]));
        assert_eq!(link.failures(), 0);
        link.close();
        handle.join().expect("server");
    }

    #[test]
    fn a_wrong_token_is_refused() {
        let (url, handle) = server(1, "secret");
        let mut link = Link::default();
        assert!(link.request(&url, "wrong", vec![1], Duration::from_secs(5)).is_err());
        assert!(!link.connected());
        handle.join().expect("server");
    }
}
