use std::fmt;

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
}
