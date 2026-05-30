use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};

use shell_core::{ProtocolEvent, Result, ShellError, TerminalSize};

const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;

const OPT_ECHO: u8 = 1;
const OPT_SUPPRESS_GO_AHEAD: u8 = 3;
const OPT_NAWS: u8 = 31;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelnetConfig {
    pub host: String,
    pub port: u16,
}

impl TelnetConfig {
    #[must_use]
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port: 23,
        }
    }

    fn validate(&self) -> Result<()> {
        if self.host.trim().is_empty() {
            return Err(ShellError::InvalidConfig(
                "Telnet host is required".to_string(),
            ));
        }
        if self.port == 0 {
            return Err(ShellError::InvalidConfig(
                "Telnet port must be greater than zero".to_string(),
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

pub struct TelnetConnection {
    stream: Arc<Mutex<TcpStream>>,
    reader_thread: Option<JoinHandle<()>>,
}

impl TelnetConnection {
    pub fn connect(
        config: TelnetConfig,
        size: TerminalSize,
    ) -> Result<(Self, mpsc::Receiver<ProtocolEvent>)> {
        config.validate()?;
        let address = config
            .endpoint()
            .to_socket_addrs()
            .map_err(|err| ShellError::Protocol(err.to_string()))?
            .next()
            .ok_or_else(|| ShellError::Protocol("Telnet host did not resolve".to_string()))?;
        let stream =
            TcpStream::connect(address).map_err(|err| ShellError::Protocol(err.to_string()))?;
        stream
            .set_nodelay(true)
            .map_err(|err| ShellError::Protocol(err.to_string()))?;
        let reader = stream
            .try_clone()
            .map_err(|err| ShellError::Protocol(err.to_string()))?;
        let stream = Arc::new(Mutex::new(stream));
        let writer = Arc::clone(&stream);
        let (sender, receiver) = mpsc::channel();

        let reader_thread = thread::Builder::new()
            .name("telnet-reader".to_string())
            .spawn(move || read_loop(reader, writer, sender, size))
            .map_err(|err| ShellError::Platform(err.to_string()))?;

        Ok((
            Self {
                stream,
                reader_thread: Some(reader_thread),
            },
            receiver,
        ))
    }

    pub fn send_input(&self, data: &[u8]) -> Result<()> {
        let escaped = escape_iac(data);
        self.stream
            .lock()
            .map_err(|_| ShellError::Protocol("Telnet stream lock poisoned".to_string()))?
            .write_all(&escaped)
            .map_err(|err| ShellError::Protocol(err.to_string()))
    }

    pub fn resize(&self, size: TerminalSize) -> Result<()> {
        self.stream
            .lock()
            .map_err(|_| ShellError::Protocol("Telnet stream lock poisoned".to_string()))?
            .write_all(&naws(size))
            .map_err(|err| ShellError::Protocol(err.to_string()))
    }

    pub fn shutdown(&self) -> Result<()> {
        self.stream
            .lock()
            .map_err(|_| ShellError::Protocol("Telnet stream lock poisoned".to_string()))?
            .shutdown(Shutdown::Both)
            .map_err(|err| ShellError::Protocol(err.to_string()))
    }
}

impl Drop for TelnetConnection {
    fn drop(&mut self) {
        let _ = self.shutdown();
        if let Some(handle) = self.reader_thread.take() {
            let _ = handle.join();
        }
    }
}

fn read_loop(
    mut reader: TcpStream,
    writer: Arc<Mutex<TcpStream>>,
    sender: mpsc::Sender<ProtocolEvent>,
    size: TerminalSize,
) {
    let mut parser = TelnetParser::new(size);
    let mut buffer = [0_u8; 8192];

    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                let _ = sender.send(ProtocolEvent::Exited(None));
                break;
            }
            Ok(read) => {
                let frame = parser.feed(&buffer[..read]);
                if !frame.response.is_empty()
                    && let Ok(mut writer) = writer.lock()
                {
                    let _ = writer.write_all(&frame.response);
                }
                if !frame.output.is_empty()
                    && sender.send(ProtocolEvent::Output(frame.output)).is_err()
                {
                    break;
                }
            }
            Err(err) => {
                let _ = sender.send(ProtocolEvent::Error(err.to_string()));
                break;
            }
        }
    }
}

fn escape_iac(data: &[u8]) -> Vec<u8> {
    let mut escaped = Vec::with_capacity(data.len());
    for byte in data {
        escaped.push(*byte);
        if *byte == IAC {
            escaped.push(IAC);
        }
    }
    escaped
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TelnetState {
    Ground,
    Iac,
    Command(u8),
    Subnegotiation,
    SubnegotiationIac,
}

#[derive(Debug)]
struct TelnetFrame {
    output: Vec<u8>,
    response: Vec<u8>,
}

#[derive(Debug)]
struct TelnetParser {
    state: TelnetState,
    size: TerminalSize,
}

impl TelnetParser {
    fn new(size: TerminalSize) -> Self {
        Self {
            state: TelnetState::Ground,
            size,
        }
    }

    fn feed(&mut self, bytes: &[u8]) -> TelnetFrame {
        let mut frame = TelnetFrame {
            output: Vec::with_capacity(bytes.len()),
            response: Vec::new(),
        };

        for byte in bytes {
            match self.state {
                TelnetState::Ground => {
                    if *byte == IAC {
                        self.state = TelnetState::Iac;
                    } else {
                        frame.output.push(*byte);
                    }
                }
                TelnetState::Iac => match *byte {
                    IAC => {
                        frame.output.push(IAC);
                        self.state = TelnetState::Ground;
                    }
                    DO | DONT | WILL | WONT => self.state = TelnetState::Command(*byte),
                    SB => self.state = TelnetState::Subnegotiation,
                    _ => self.state = TelnetState::Ground,
                },
                TelnetState::Command(command) => {
                    frame.response.extend(self.negotiate(command, *byte));
                    self.state = TelnetState::Ground;
                }
                TelnetState::Subnegotiation => {
                    if *byte == IAC {
                        self.state = TelnetState::SubnegotiationIac;
                    }
                }
                TelnetState::SubnegotiationIac => {
                    self.state = if *byte == SE {
                        TelnetState::Ground
                    } else {
                        TelnetState::Subnegotiation
                    };
                }
            }
        }

        frame
    }

    fn negotiate(&self, command: u8, option: u8) -> Vec<u8> {
        match (command, option) {
            (DO, OPT_NAWS) => {
                let mut response = vec![IAC, WILL, OPT_NAWS];
                response.extend(naws(self.size));
                response
            }
            (DO, OPT_SUPPRESS_GO_AHEAD) => vec![IAC, WILL, option],
            (DO, _) => vec![IAC, WONT, option],
            (WILL, OPT_ECHO | OPT_SUPPRESS_GO_AHEAD) => vec![IAC, DO, option],
            (WILL, _) => vec![IAC, DONT, option],
            _ => Vec::new(),
        }
    }
}

fn naws(size: TerminalSize) -> Vec<u8> {
    let cols = size.cols.to_be_bytes();
    let rows = size.rows.to_be_bytes();
    vec![
        IAC, SB, OPT_NAWS, cols[0], cols[1], rows[0], rows[1], IAC, SE,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_plain_text() {
        let mut parser = TelnetParser::new(TerminalSize::new(80, 24));
        let frame = parser.feed(b"login: ");

        assert_eq!(frame.output, b"login: ");
        assert!(frame.response.is_empty());
    }

    #[test]
    fn escapes_literal_iac() {
        assert_eq!(escape_iac(&[1, IAC, 2]), vec![1, IAC, IAC, 2]);
    }

    #[test]
    fn negotiates_naws() {
        let mut parser = TelnetParser::new(TerminalSize::new(120, 32));
        let frame = parser.feed(&[IAC, DO, OPT_NAWS]);

        assert!(frame.output.is_empty());
        assert_eq!(&frame.response[..3], &[IAC, WILL, OPT_NAWS]);
        assert!(frame.response.ends_with(&[IAC, SE]));
    }

    #[test]
    fn filters_subnegotiation_bytes() {
        let mut parser = TelnetParser::new(TerminalSize::new(80, 24));
        let frame = parser.feed(&[b'a', IAC, SB, 1, 2, IAC, SE, b'b']);

        assert_eq!(frame.output, b"ab");
    }
}
