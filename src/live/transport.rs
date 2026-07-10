use std::{
    io::{Read, Write},
    net::TcpStream,
    time::Duration,
};

use thiserror::Error;

const READ_TIMEOUT: Duration = Duration::from_millis(100);
const WRITE_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportConfig {
    Serial { port: String, baud: u32 },
    Tcp { address: String },
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self::Tcp {
            address: "127.0.0.1:19090".to_owned(),
        }
    }
}

impl TransportConfig {
    pub fn validate(&self) -> Result<(), TransportError> {
        match self {
            Self::Serial { port, baud } => {
                if port.trim().is_empty() {
                    return Err(TransportError::InvalidConfig(
                        "serial port must not be empty".to_owned(),
                    ));
                }
                if *baud == 0 {
                    return Err(TransportError::InvalidConfig(
                        "serial baud must be greater than zero".to_owned(),
                    ));
                }
            }
            Self::Tcp { address } => {
                if address.trim().is_empty() || !address.contains(':') {
                    return Err(TransportError::InvalidConfig(
                        "TCP address must use host:port".to_owned(),
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn connect(&self) -> Result<TransportStream, TransportError> {
        self.validate()?;
        match self {
            Self::Serial { port, baud } => {
                let serial = serialport::new(port, *baud)
                    .data_bits(serialport::DataBits::Eight)
                    .stop_bits(serialport::StopBits::One)
                    .parity(serialport::Parity::None)
                    .flow_control(serialport::FlowControl::None)
                    .timeout(READ_TIMEOUT)
                    .open()?;
                Ok(TransportStream::Serial(serial))
            }
            Self::Tcp { address } => {
                let stream = TcpStream::connect(address)?;
                stream.set_nodelay(true)?;
                stream.set_read_timeout(Some(READ_TIMEOUT))?;
                stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
                Ok(TransportStream::Tcp(stream))
            }
        }
    }
}

pub enum TransportStream {
    Serial(Box<dyn serialport::SerialPort>),
    Tcp(TcpStream),
}

impl Read for TransportStream {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Serial(stream) => stream.read(bytes),
            Self::Tcp(stream) => stream.read(bytes),
        }
    }
}

impl Write for TransportStream {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Serial(stream) => stream.write(bytes),
            Self::Tcp(stream) => stream.write(bytes),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Serial(stream) => stream.flush(),
            Self::Tcp(stream) => stream.flush(),
        }
    }
}

pub fn available_serial_ports() -> Result<Vec<serialport::SerialPortInfo>, TransportError> {
    let mut ports = serialport::available_ports()?;
    ports.sort_by(|left, right| left.port_name.cmp(&right.port_name));
    Ok(ports)
}

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("invalid live transport configuration: {0}")]
    InvalidConfig(String),
    #[error("live transport I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serial transport error: {0}")]
    Serial(#[from] serialport::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_config_rejects_empty_serial_port_and_zero_baud() {
        assert!(TransportConfig::Serial {
            port: String::new(),
            baud: 921_600,
        }
        .validate()
        .is_err());
        assert!(TransportConfig::Serial {
            port: "COM3".to_owned(),
            baud: 0,
        }
        .validate()
        .is_err());
    }
}
