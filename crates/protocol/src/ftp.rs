/// 最小化 FTP 客户端连接。
///
/// 向终端视图提供命令行式交互界面。
/// 支持 FTP 控制通道，以及用于 LIST/RETR/STOR 的被动模式数据通道。
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::Duration;

use shell_core::ProtocolEvent;
use tracing::warn;

use crate::output::{OutputSender, output_channel};

const MAX_REPLY_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct FtpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    /// 密码不应以明文持久化保存；这里只在会话生命周期内保存在内存中。
    pub password: String,
}

impl FtpConfig {
    #[must_use]
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port: 21,
            username: "anonymous".to_string(),
            password: "guest@example.com".to_string(),
        }
    }
}

pub struct FtpConnection {
    input_tx: mpsc::Sender<Vec<u8>>,
    shutdown_stream: TcpStream,
    shutdown_requested: Arc<AtomicBool>,
}

impl FtpConnection {
    /// 连接 FTP 服务器并启动输入输出线程。
    ///
    /// 返回连接句柄，以及供 UI 轮询的事件接收端。
    pub fn connect(config: FtpConfig) -> anyhow::Result<(Self, mpsc::Receiver<ProtocolEvent>)> {
        let addr = format!("{}:{}", config.host, config.port);
        let stream = TcpStream::connect(&addr)?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        let shutdown_stream = stream.try_clone()?;

        let shutdown_requested = Arc::new(AtomicBool::new(false));
        let (event_tx, event_rx) = output_channel(Arc::clone(&shutdown_requested));
        let (input_tx, input_rx) = mpsc::channel::<Vec<u8>>();

        thread::spawn(move || {
            run_ftp_session(stream, config, input_rx, event_tx);
        });

        Ok((
            Self {
                input_tx,
                shutdown_stream,
                shutdown_requested,
            },
            event_rx,
        ))
    }

    pub fn send_input(&self, data: &[u8]) -> anyhow::Result<()> {
        self.input_tx.send(data.to_vec())?;
        Ok(())
    }

    pub fn shutdown(&self) -> anyhow::Result<()> {
        self.shutdown_requested.store(true, Ordering::Relaxed);
        let _ = self.input_tx.send(b"quit\r\n".to_vec());
        let _ = self.shutdown_stream.shutdown(Shutdown::Both);
        Ok(())
    }
}

impl Drop for FtpConnection {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn emit(tx: &OutputSender, text: &str) {
    let _ = tx.send(ProtocolEvent::Output(text.as_bytes().to_vec()));
}

/// 从读取器读取一个完整的 FTP 回复，回复可能包含多行。
fn read_reply(reader: &mut impl BufRead) -> io::Result<String> {
    let mut full = String::new();
    let mut multiline_code: Option<[u8; 3]> = None;
    loop {
        let mut line = String::new();
        let remaining = MAX_REPLY_BYTES - full.len();
        let read = Read::by_ref(reader)
            .take(remaining as u64 + 1)
            .read_line(&mut line)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "FTP server disconnected during its reply",
            ));
        }
        if read > remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "FTP reply exceeds the size limit",
            ));
        }
        if !line.ends_with('\n') {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "FTP server disconnected during its reply",
            ));
        }
        full.push_str(&line);
        let bytes = line.as_bytes();
        if let Some(code) = multiline_code {
            // RFC 959: only the original code followed by a space ends a
            // multiline reply. Intermediate lines may be arbitrary text.
            if bytes.starts_with(&code) && bytes.get(3) == Some(&b' ') {
                return Ok(full);
            }
        } else {
            if bytes.len() < 4 || !bytes[..3].iter().all(u8::is_ascii_digit) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Invalid FTP reply code",
                ));
            }
            match bytes[3] {
                b' ' => return Ok(full),
                b'-' => multiline_code = Some([bytes[0], bytes[1], bytes[2]]),
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Invalid FTP reply separator",
                    ));
                }
            }
        }
    }
}

/// 解析 PASV 回复，并返回需要连接的数据通道 `(host, port)`。
fn parse_pasv(reply: &str) -> Option<(String, u16)> {
    let start = reply.find('(')?;
    let end = reply.rfind(')')?;
    let nums: Vec<u8> = reply[start + 1..end]
        .split(',')
        .map(|s| s.trim().parse::<u8>())
        .collect::<Result<_, _>>()
        .ok()?;
    if nums.len() != 6 {
        return None;
    }
    let host = format!("{}.{}.{}.{}", nums[0], nums[1], nums[2], nums[3]);
    let port = (u16::from(nums[4]) << 8) | u16::from(nums[5]);
    Some((host, port))
}

fn send_cmd(writer: &mut TcpStream, cmd: &str) -> io::Result<()> {
    writer.write_all(cmd.as_bytes())?;
    writer.flush()
}

fn run_ftp_session(
    stream: TcpStream,
    config: FtpConfig,
    input_rx: mpsc::Receiver<Vec<u8>>,
    event_tx: OutputSender,
) {
    let mut writer = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            let _ = event_tx.send(ProtocolEvent::Error(e.to_string()));
            return;
        }
    };
    let mut reader = BufReader::new(stream);

    // 读取服务器欢迎信息。
    match read_reply(&mut reader) {
        Ok(reply) => emit(&event_tx, &reply),
        Err(e) => {
            let _ = event_tx.send(ProtocolEvent::Error(e.to_string()));
            return;
        }
    }

    // 登录认证。
    if send_cmd(&mut writer, &format!("USER {}\r\n", config.username)).is_err() {
        return;
    }
    match read_reply(&mut reader) {
        Ok(reply) => {
            emit(&event_tx, &reply);
            if reply.starts_with("331") {
                if send_cmd(&mut writer, &format!("PASS {}\r\n", config.password)).is_err() {
                    return;
                }
                match read_reply(&mut reader) {
                    Ok(r) => emit(&event_tx, &r),
                    Err(e) => {
                        let _ = event_tx.send(ProtocolEvent::Error(e.to_string()));
                        return;
                    }
                }
            }
        }
        Err(e) => {
            let _ = event_tx.send(ProtocolEvent::Error(e.to_string()));
            return;
        }
    }

    emit(&event_tx, "ftp> ");

    let mut line_buf = Vec::<u8>::new();

    while !event_tx.is_cancelled() {
        // 轮询用户输入。
        match input_rx.try_recv() {
            Ok(bytes) => {
                for &b in &bytes {
                    match b {
                        b'\r' | b'\n' => {
                            let cmd = String::from_utf8_lossy(&line_buf).trim().to_string();
                            line_buf.clear();
                            emit(&event_tx, "\r\n");

                            if cmd.is_empty() {
                                emit(&event_tx, "ftp> ");
                                continue;
                            }

                            if cmd.eq_ignore_ascii_case("quit")
                                || cmd.eq_ignore_ascii_case("bye")
                                || cmd.eq_ignore_ascii_case("exit")
                            {
                                let _ = send_cmd(&mut writer, "QUIT\r\n");
                                if let Ok(r) = read_reply(&mut reader) {
                                    emit(&event_tx, &r);
                                }
                                let _ = event_tx.send(ProtocolEvent::Exited(Some(0)));
                                return;
                            }

                            handle_command(&cmd, &mut writer, &mut reader, &event_tx, &config.host);
                            emit(&event_tx, "ftp> ");
                        }
                        // 退格 / DEL。
                        0x7f | 0x08 => {
                            if !line_buf.is_empty() {
                                line_buf.pop();
                                emit(&event_tx, "\x08 \x08");
                            }
                        }
                        b => {
                            line_buf.push(b);
                            // 回显输入字符。
                            emit(&event_tx, &String::from_utf8_lossy(&[b]));
                        }
                    }
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => return,
            Err(mpsc::TryRecvError::Empty) => {
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn handle_command(
    cmd: &str,
    writer: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    event_tx: &OutputSender,
    _host: &str,
) {
    let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
    let verb = parts[0].to_uppercase();
    let arg = parts.get(1).copied().unwrap_or("").trim();

    match verb.as_str() {
        "HELP" | "?" => {
            emit(
                event_tx,
                "Available commands:\r\n  ls/dir/list [path]  pwd  cd <path>\r\n  \
                 get <file>  put <file>  del <file>\r\n  \
                 mkdir <dir>  rmdir <dir>  rename <old> <new>\r\n  \
                 syst  noop  quote <raw>  quit\r\n",
            );
        }
        "PWD" => send_simple(writer, reader, event_tx, "PWD\r\n"),
        "SYST" => send_simple(writer, reader, event_tx, "SYST\r\n"),
        "NOOP" => send_simple(writer, reader, event_tx, "NOOP\r\n"),
        "CD" | "CWD" => {
            send_simple(writer, reader, event_tx, &format!("CWD {arg}\r\n"));
        }
        "MKDIR" | "MKD" => {
            send_simple(writer, reader, event_tx, &format!("MKD {arg}\r\n"));
        }
        "RMDIR" | "RMD" => {
            send_simple(writer, reader, event_tx, &format!("RMD {arg}\r\n"));
        }
        "DEL" | "DELETE" | "DELE" => {
            send_simple(writer, reader, event_tx, &format!("DELE {arg}\r\n"));
        }
        "RENAME" | "RNFR" => {
            let sub: Vec<&str> = arg.splitn(2, ' ').collect();
            let from = sub.first().copied().unwrap_or("");
            let to = sub.get(1).copied().unwrap_or("");
            send_simple(writer, reader, event_tx, &format!("RNFR {from}\r\n"));
            send_simple(writer, reader, event_tx, &format!("RNTO {to}\r\n"));
        }
        "QUOTE" => {
            send_simple(writer, reader, event_tx, &format!("{arg}\r\n"));
        }
        "LS" | "DIR" | "LIST" => {
            data_cmd(writer, reader, event_tx, "LIST", arg);
        }
        "NLST" => {
            data_cmd(writer, reader, event_tx, "NLST", arg);
        }
        "GET" | "RETR" => {
            if arg.is_empty() {
                emit(event_tx, "Usage: get <filename>\r\n");
            } else {
                data_cmd(writer, reader, event_tx, "RETR", arg);
            }
        }
        "PUT" | "STOR" => {
            emit(
                event_tx,
                "STOR via terminal is not supported. Use an SFTP client for file transfers.\r\n",
            );
        }
        _ => {
            // 其它命令按原始 FTP 命令透传。
            send_simple(writer, reader, event_tx, &format!("{cmd}\r\n"));
        }
    }
}

/// 发送不需要数据通道的简单 FTP 命令，并输出响应。
fn send_simple(
    writer: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    event_tx: &OutputSender,
    cmd: &str,
) {
    if writer.write_all(cmd.as_bytes()).is_err() {
        emit(event_tx, "Connection error.\r\n");
        return;
    }
    match read_reply(reader) {
        Ok(reply) => emit(event_tx, &reply),
        Err(_) => emit(event_tx, "Connection error.\r\n"),
    }
}

/// 用被动模式发送需要数据通道的 FTP 命令（LIST、RETR 等）。
fn data_cmd(
    writer: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    event_tx: &OutputSender,
    verb: &str,
    arg: &str,
) {
    // 进入二进制传输模式。
    let _ = writer.write_all(b"TYPE I\r\n");
    let _ = read_reply(reader);

    // 请求被动模式。
    if writer.write_all(b"PASV\r\n").is_err() {
        emit(event_tx, "Connection error.\r\n");
        return;
    }
    let pasv_reply = match read_reply(reader) {
        Ok(r) => r,
        Err(_) => {
            emit(event_tx, "PASV failed.\r\n");
            return;
        }
    };
    if !pasv_reply.starts_with("227") {
        emit(event_tx, &pasv_reply);
        return;
    }
    let (data_host, data_port) = match parse_pasv(&pasv_reply) {
        Some(v) => v,
        None => {
            emit(event_tx, "Failed to parse PASV response.\r\n");
            return;
        }
    };

    // Establish the passive data connection before requesting the transfer.
    // Some servers wait for it before sending their preliminary 1xx reply.
    let data_addr = format!("{data_host}:{data_port}");
    let mut data_stream = match TcpStream::connect(&data_addr) {
        Ok(s) => s,
        Err(e) => {
            warn!("FTP data connection failed: {e}");
            emit(event_tx, &format!("Data connection failed: {e}\r\n"));
            return;
        }
    };
    if data_stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .is_err()
    {
        emit(event_tx, "Failed to configure the FTP data connection.\r\n");
        return;
    }

    // 发送实际数据命令。
    let full_cmd = if arg.is_empty() {
        format!("{verb}\r\n")
    } else {
        format!("{verb} {arg}\r\n")
    };
    if writer.write_all(full_cmd.as_bytes()).is_err() {
        emit(event_tx, "Connection error.\r\n");
        return;
    }

    // 读取 1xx 初步响应。
    let pre = match read_reply(reader) {
        Ok(r) => r,
        Err(_) => {
            emit(event_tx, "Connection error.\r\n");
            return;
        }
    };
    if !pre.starts_with('1') {
        emit(event_tx, &pre);
        return;
    }

    // 将数据流输出到终端。
    let mut buf = [0u8; 4096];
    while !event_tx.is_cancelled() {
        match data_stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                // 为终端显示把裸 LF 规范化为 CRLF。
                let chunk = normalize_newlines(&buf[..n]);
                if event_tx.send(ProtocolEvent::Output(chunk)).is_err() {
                    return;
                }
            }
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                continue;
            }
            Err(_) => break,
        }
    }

    // 读取最终响应。
    match read_reply(reader) {
        Ok(reply) => emit(event_tx, &reply),
        Err(_) => emit(event_tx, "Connection error.\r\n"),
    }
}

fn normalize_newlines(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 16);
    let mut prev = 0u8;
    for &b in data {
        if b == b'\n' && prev != b'\r' {
            out.push(b'\r');
        }
        out.push(b);
        prev = b;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pasv_response() {
        let reply = "227 Entering Passive Mode (192,168,1,1,100,200)\r\n";
        let result = parse_pasv(reply).unwrap();
        assert_eq!(result.0, "192.168.1.1");
        assert_eq!(result.1, 100 * 256 + 200);
    }

    #[test]
    fn normalize_bare_lf() {
        let input = b"line1\nline2\r\nline3\n";
        let out = normalize_newlines(input);
        assert_eq!(out, b"line1\r\nline2\r\nline3\r\n");
    }

    #[test]
    fn disconnected_or_truncated_replies_return_eof() {
        for bytes in [
            b"".as_slice(),
            b"220-incomplete\r\n",
            b"220 missing newline",
        ] {
            let mut reader = io::Cursor::new(bytes);
            assert_eq!(
                read_reply(&mut reader).unwrap_err().kind(),
                io::ErrorKind::UnexpectedEof
            );
        }
    }

    #[test]
    fn multiline_reply_consumes_only_its_matching_terminator() {
        let reply = "220-Welcome\r\nArbitrary text\r\n123 unrelated code\r\n220 Ready\r\n";
        let mut reader = io::Cursor::new(format!("{reply}230 Logged in\r\n"));
        assert_eq!(read_reply(&mut reader).unwrap(), reply);
        assert_eq!(read_reply(&mut reader).unwrap(), "230 Logged in\r\n");
    }

    #[test]
    fn malformed_and_oversized_replies_are_rejected() {
        for reply in [
            "garbage\r\n".to_string(),
            format!("220 {}\r\n", "x".repeat(MAX_REPLY_BYTES)),
        ] {
            assert!(read_reply(&mut io::Cursor::new(reply)).is_err());
        }
        assert!(parse_pasv("227 (127,0,0,bad,1,10,20)").is_none());
    }

    #[test]
    fn passive_transfer_connects_before_waiting_for_the_preliminary_reply() {
        use std::net::TcpListener;
        use std::time::Instant;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut control = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        control
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let (mut server, _) = listener.accept().unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let worker = thread::spawn(move || {
            let mut reader = BufReader::new(server.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "TYPE I\r\n");
            server.write_all(b"200 Binary\r\n").unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "PASV\r\n");
            let data_listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = data_listener.local_addr().unwrap().port();
            write!(
                server,
                "227 Passive (127,0,0,1,{},{})\r\n",
                port >> 8,
                port & 255
            )
            .unwrap();
            data_listener.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut data = loop {
                match data_listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "client never opened the passive data connection"
                        );
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("data accept failed: {error}"),
                }
            };
            line.clear();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "LIST\r\n");
            server.write_all(b"150 Listing\r\n").unwrap();
            data.write_all(b"example.txt\n").unwrap();
            drop(data);
            server.write_all(b"226 Complete\r\n").unwrap();
        });
        let mut reader = BufReader::new(control.try_clone().unwrap());
        let (sender, receiver) = output_channel(Arc::new(AtomicBool::new(false)));
        data_cmd(&mut control, &mut reader, &sender, "LIST", "");
        drop(sender);
        worker.join().unwrap();
        let output: Vec<u8> = receiver
            .into_iter()
            .flat_map(|event| {
                if let ProtocolEvent::Output(bytes) = event {
                    bytes
                } else {
                    Vec::new()
                }
            })
            .collect();
        assert_eq!(output, b"example.txt\r\n226 Complete\r\n");
    }
}
