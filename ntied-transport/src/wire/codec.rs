use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

const ADDR_TYPE_IPV4: u8 = 4;
const ADDR_TYPE_IPV6: u8 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecError {
    UnexpectedEnd,
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEnd => f.write_str("unexpected end of buffer"),
        }
    }
}

impl std::error::Error for CodecError {}

pub struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    pub fn read_u8(&mut self) -> Result<u8, CodecError> {
        if self.buf.is_empty() {
            return Err(CodecError::UnexpectedEnd);
        }
        let val = self.buf[0];
        self.buf = &self.buf[1..];
        Ok(val)
    }

    pub fn read_u16(&mut self) -> Result<u16, CodecError> {
        let bytes = self.read_array::<2>()?;
        Ok(u16::from_be_bytes(bytes))
    }

    pub fn read_u32(&mut self) -> Result<u32, CodecError> {
        let bytes = self.read_array::<4>()?;
        Ok(u32::from_be_bytes(bytes))
    }

    pub fn read_u64(&mut self) -> Result<u64, CodecError> {
        let bytes = self.read_array::<8>()?;
        Ok(u64::from_be_bytes(bytes))
    }

    pub fn read_array<const N: usize>(&mut self) -> Result<[u8; N], CodecError> {
        if self.buf.len() < N {
            return Err(CodecError::UnexpectedEnd);
        }
        let arr: [u8; N] = self.buf[..N].try_into().unwrap();
        self.buf = &self.buf[N..];
        Ok(arr)
    }

    pub fn read_bytes(&mut self, n: usize) -> Result<&'a [u8], CodecError> {
        if self.buf.len() < n {
            return Err(CodecError::UnexpectedEnd);
        }
        let (result, rest) = self.buf.split_at(n);
        self.buf = rest;
        Ok(result)
    }

    pub fn remaining(&self) -> &'a [u8] {
        self.buf
    }

    pub fn remaining_len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn read_socket_addr(&mut self) -> Result<SocketAddr, CodecError> {
        let addr_type = self.read_u8()?;
        match addr_type {
            ADDR_TYPE_IPV4 => {
                let octets: [u8; 4] = self.read_array()?;
                let port = self.read_u16()?;
                Ok(SocketAddr::from((Ipv4Addr::from(octets), port)))
            }
            ADDR_TYPE_IPV6 => {
                let octets: [u8; 16] = self.read_array()?;
                let port = self.read_u16()?;
                Ok(SocketAddr::from((Ipv6Addr::from(octets), port)))
            }
            _ => Err(CodecError::UnexpectedEnd),
        }
    }
}

pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buf: Vec::with_capacity(capacity),
        }
    }

    pub fn write_u8(&mut self, val: u8) {
        self.buf.push(val);
    }

    pub fn write_u16(&mut self, val: u16) {
        self.buf.extend_from_slice(&val.to_be_bytes());
    }

    pub fn write_u32(&mut self, val: u32) {
        self.buf.extend_from_slice(&val.to_be_bytes());
    }

    pub fn write_u64(&mut self, val: u64) {
        self.buf.extend_from_slice(&val.to_be_bytes());
    }

    pub fn write_bytes(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.buf
    }

    pub fn write_socket_addr(&mut self, addr: &SocketAddr) {
        match addr {
            SocketAddr::V4(v4) => {
                self.write_u8(ADDR_TYPE_IPV4);
                self.write_bytes(&v4.ip().octets());
                self.write_u16(v4.port());
            }
            SocketAddr::V6(v6) => {
                self.write_u8(ADDR_TYPE_IPV6);
                self.write_bytes(&v6.ip().octets());
                self.write_u16(v6.port());
            }
        }
    }
}
