use std::io::{Read, Write};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serialport::SerialPort;
use shell_core::{ProtocolEvent, Result, ShellError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialConfig {
    pub port_name: String,
    pub baud_rate: u32,
}

impl SerialConfig {
    #[must_use]
    pub fn new(port_name: impl Into<String>) -> Self {
        Self {
            port_name: port_name.into(),
            baud_rate: 115_200,
        }
    }

    fn validate(&self) -> Result<()> {
        if self.port_name.trim().is_empty() {
            return Err(ShellError::InvalidConfig(
                "Serial port name is required".to_string(),
            ));
        }
        if self.baud_rate == 0 {
            return Err(ShellError::InvalidConfig(
                "Serial baud rate must be greater than zero".to_string(),
            ));
        }
        Ok(())
    }
}

pub struct SerialConnection {
    port: Arc<Mutex<Box<dyn SerialPort>>>,
    shutdown_requested: Arc<AtomicBool>,
    reader_thread: Option<JoinHandle<()>>,
}

impl SerialConnection {
    pub fn open(config: SerialConfig) -> Result<(Self, mpsc::Receiver<ProtocolEvent>)> {
        config.validate()?;
        let port = serialport::new(&config.port_name, config.baud_rate)
            .timeout(Duration::from_millis(100))
            .open()
            .map_err(|err| ShellError::Protocol(err.to_string()))?;
        let reader = port
            .try_clone()
            .map_err(|err| ShellError::Protocol(err.to_string()))?;
        let port = Arc::new(Mutex::new(port));
        let shutdown_requested = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::channel();
        let shutdown_for_reader = Arc::clone(&shutdown_requested);

        let reader_thread = thread::Builder::new()
            .name("serial-reader".to_string())
            .spawn(move || read_loop(reader, sender, shutdown_for_reader))
            .map_err(|err| ShellError::Platform(err.to_string()))?;

        Ok((
            Self {
                port,
                shutdown_requested,
                reader_thread: Some(reader_thread),
            },
            receiver,
        ))
    }

    pub fn send_input(&self, data: &[u8]) -> Result<()> {
        self.port
            .lock()
            .map_err(|_| ShellError::Protocol("Serial port lock poisoned".to_string()))?
            .write_all(data)
            .map_err(|err| ShellError::Protocol(err.to_string()))
    }

    pub fn shutdown(&self) -> Result<()> {
        self.shutdown_requested.store(true, Ordering::Relaxed);
        Ok(())
    }
}

impl Drop for SerialConnection {
    fn drop(&mut self) {
        let _ = self.shutdown();
        if let Some(handle) = self.reader_thread.take() {
            let _ = handle.join();
        }
    }
}

pub fn available_serial_ports() -> Result<Vec<String>> {
    serialport::available_ports()
        .map(|ports| ports.into_iter().map(|port| port.port_name).collect())
        .map_err(|err| ShellError::Protocol(err.to_string()))
}

fn read_loop(
    mut reader: Box<dyn SerialPort>,
    sender: mpsc::Sender<ProtocolEvent>,
    shutdown_requested: Arc<AtomicBool>,
) {
    let mut buffer = [0_u8; 4096];
    loop {
        if shutdown_requested.load(Ordering::Relaxed) {
            let _ = sender.send(ProtocolEvent::Exited(Some(0)));
            break;
        }

        match reader.read(&mut buffer) {
            Ok(0) => {}
            Ok(read) => {
                if sender
                    .send(ProtocolEvent::Output(buffer[..read].to_vec()))
                    .is_err()
                {
                    break;
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::TimedOut => {}
            Err(err) => {
                let _ = sender.send(ProtocolEvent::Error(err.to_string()));
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_common_embedded_baud_rate() {
        let config = SerialConfig::new("COM1");

        assert_eq!(config.port_name, "COM1");
        assert_eq!(config.baud_rate, 115_200);
    }

    #[test]
    fn rejects_empty_port_name() {
        let config = SerialConfig::new(" ");

        assert!(config.validate().is_err());
    }
}
