use std::io::Read;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};

use shell_core::{ProtocolEvent, Result, ShellError, TerminalSize};
use shell_platform::{LocalPty, PtyConfig};

pub struct PtyConnection {
    // `Option` 让 shutdown 只获取一次所有权。互斥锁保护来自 UI 回调的写入/尺寸调整，
    // 读取线程只持有 `LocalPty::spawn` 返回的克隆读取器。
    pty: Arc<Mutex<Option<LocalPty>>>,
    reader_thread: Option<JoinHandle<()>>,
}

impl PtyConnection {
    pub fn spawn(
        config: PtyConfig,
        reader_thread_name: impl Into<String>,
    ) -> Result<(Self, mpsc::Receiver<ProtocolEvent>)> {
        let (pty, mut reader) = LocalPty::spawn(config)?;
        let (sender, receiver) = mpsc::channel();

        let reader_thread = thread::Builder::new()
            .name(reader_thread_name.into())
            .spawn(move || read_loop(&mut reader, sender))
            .map_err(|err| ShellError::Platform(err.to_string()))?;

        Ok((
            Self {
                pty: Arc::new(Mutex::new(Some(pty))),
                reader_thread: Some(reader_thread),
            },
            receiver,
        ))
    }

    pub fn send_input(&self, data: &[u8]) -> Result<()> {
        let mut pty = self
            .pty
            .lock()
            .map_err(|_| ShellError::Platform("PTY lock poisoned".to_string()))?;
        let Some(pty) = pty.as_mut() else {
            return Err(ShellError::Platform("PTY is closed".to_string()));
        };
        pty.write_all(data)
    }

    pub fn resize(&self, size: TerminalSize) -> Result<()> {
        let mut pty = self
            .pty
            .lock()
            .map_err(|_| ShellError::Platform("PTY lock poisoned".to_string()))?;
        let Some(pty) = pty.as_mut() else {
            return Err(ShellError::Platform("PTY is closed".to_string()));
        };
        pty.resize(size)
    }

    pub fn shutdown(&self) -> Result<()> {
        let mut pty = self
            .pty
            .lock()
            .map_err(|_| ShellError::Platform("PTY lock poisoned".to_string()))?;
        let Some(mut pty) = pty.take() else {
            return Ok(());
        };
        let result = pty.shutdown();
        drop(pty);
        result
    }
}

impl Drop for PtyConnection {
    fn drop(&mut self) {
        let _ = self.shutdown();
        if let Some(handle) = self.reader_thread.take() {
            let _ = handle.join();
        }
    }
}

fn read_loop(reader: &mut Box<dyn Read + Send>, sender: mpsc::Sender<ProtocolEvent>) {
    // 将阻塞 PTY 读取转换为 UI 使用的少量事件。EOF/error 之后是关闭、更新状态还是
    // 重连，由 UI 层根据页面类型决定。
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                let _ = sender.send(ProtocolEvent::Exited(None));
                break;
            }
            Ok(read) => {
                if sender
                    .send(ProtocolEvent::Output(buffer[..read].to_vec()))
                    .is_err()
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
