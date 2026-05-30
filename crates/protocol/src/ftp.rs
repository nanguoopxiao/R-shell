/// 最小化 FTP 客户端连接。
///
/// 向终端视图提供命令行式交互界面。
/// 支持 FTP 控制通道，以及用于 LIST/RETR/STOR 的被动模式数据通道。
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use shell_core::ProtocolEvent;
use tracing::warn;

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

        let (event_tx, event_rx) = mpsc::channel::<ProtocolEvent>();
        let (input_tx, input_rx) = mpsc::channel::<Vec<u8>>();

        thread::spawn(move || {
            run_ftp_session(stream, config, input_rx, event_tx);
        });

        Ok((
            Self {
                input_tx,
                shutdown_stream,
            },
            event_rx,
        ))
    }

    pub fn send_input(&self, data: &[u8]) -> anyhow::Result<()> {
        self.input_tx.send(data.to_vec())?;
        Ok(())
    }

    pub fn shutdown(&self) -> anyhow::Result<()> {
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

fn emit(tx: &mpsc::Sender<ProtocolEvent>, text: &str) {
    let _ = tx.send(ProtocolEvent::Output(text.as_bytes().to_vec()));
}

/// 从读取器读取一个完整的 FTP 回复，回复可能包含多行。
fn read_reply(reader: &mut BufReader<TcpStream>) -> io::Result<String> {
    let mut full = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        full.push_str(&line);
        // 单行回复形如 "NNN text"，多行续行形如 "NNN-text"；多行回复的最后
        // 一行重新变为 "NNN text"，也就是第 3 位没有连字符。
        if line.len() >= 4 && line.as_bytes().get(3) != Some(&b'-') {
            break;
        }
    }
    Ok(full)
}

/// 解析 PASV 回复，并返回需要连接的数据通道 `(host, port)`。
fn parse_pasv(reply: &str) -> Option<(String, u16)> {
    let start = reply.find('(')?;
    let end = reply.rfind(')')?;
    let nums: Vec<u8> = reply[start + 1..end]
        .split(',')
        .filter_map(|s| s.trim().parse::<u8>().ok())
        .collect();
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
    event_tx: mpsc::Sender<ProtocolEvent>,
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

    loop {
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
    event_tx: &mpsc::Sender<ProtocolEvent>,
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
    event_tx: &mpsc::Sender<ProtocolEvent>,
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
    event_tx: &mpsc::Sender<ProtocolEvent>,
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

    // 连接数据通道。
    let data_addr = format!("{data_host}:{data_port}");
    let mut data_stream = match TcpStream::connect(&data_addr) {
        Ok(s) => s,
        Err(e) => {
            warn!("FTP data connection failed: {e}");
            emit(event_tx, &format!("Data connection failed: {e}\r\n"));
            return;
        }
    };

    // 将数据流输出到终端。
    let mut buf = [0u8; 4096];
    loop {
        match data_stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                // 为终端显示把裸 LF 规范化为 CRLF。
                let chunk = normalize_newlines(&buf[..n]);
                let _ = event_tx.send(ProtocolEvent::Output(chunk));
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
}
