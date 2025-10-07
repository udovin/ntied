use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

pub struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    pub fn read_u8(&mut self) -> Result<u8, String> {
        if self.data.is_empty() {
            return Err("Unexpected end of data".to_string());
        }
        let value = self.data[0];
        self.data = &self.data[1..];
        Ok(value)
    }

    pub fn read_u16(&mut self) -> Result<u16, String> {
        if self.data.len() < 2 {
            return Err("Unexpected end of data".to_string());
        }
        let bytes: [u8; 2] = self.data[..2].try_into().unwrap();
        let value = u16::from_be_bytes(bytes);
        self.data = &self.data[2..];
        Ok(value)
    }

    pub fn read_u32(&mut self) -> Result<u32, String> {
        if self.data.len() < 4 {
            return Err("Unexpected end of data".to_string());
        }
        let bytes: [u8; 4] = self.data[..4].try_into().unwrap();
        let value = u32::from_be_bytes(bytes);
        self.data = &self.data[4..];
        Ok(value)
    }

    pub fn read_bytes(&mut self) -> Result<Vec<u8>, String> {
        let len = self.read_u16()? as usize;
        if self.data.len() < len {
            return Err("Unexpected end of data".to_string());
        }
        let value = Vec::from(&self.data[..len]);
        self.data = &self.data[len..];
        Ok(value)
    }

    pub fn read_array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        if self.data.len() < N {
            return Err("Unexpected end of data".to_string());
        }
        let bytes: [u8; N] = self.data[..N].try_into().unwrap();
        self.data = &self.data[N..];
        Ok(bytes)
    }

    pub fn read_string(&mut self) -> Result<String, String> {
        let len = self.read_u16()? as usize;
        if self.data.len() < len {
            return Err("Unexpected end of data".to_string());
        }
        let value = String::from_utf8(self.data[..len].to_vec())
            .map_err(|_| "Invalid UTF-8".to_string())?;
        self.data = &self.data[len..];
        Ok(value)
    }

    pub fn read_ip_addr(&mut self) -> Result<IpAddr, String> {
        let version = self.read_u8()?;
        match version {
            4 => {
                if self.data.len() < 4 {
                    return Err("Unexpected end of data".to_string());
                }
                let bytes: [u8; 4] = self.data[0..4].try_into().unwrap();
                let ip = Ipv4Addr::from(bytes);
                self.data = &self.data[4..];
                Ok(IpAddr::V4(ip))
            }
            6 => {
                if self.data.len() < 16 {
                    return Err("Unexpected end of data".to_string());
                }
                let bytes: [u8; 16] = self.data[0..16].try_into().unwrap();
                let ip = Ipv6Addr::from(bytes);
                self.data = &self.data[16..];
                Ok(IpAddr::V6(ip))
            }
            _ => Err("Unsupported IP address type".to_string()),
        }
    }

    pub fn read_socket_addr(&mut self) -> Result<SocketAddr, String> {
        let ip = self.read_ip_addr()?;
        let port = self.read_u16()?;
        Ok(SocketAddr::new(ip, port))
    }
}

pub struct Writer<'a> {
    data: &'a mut Vec<u8>,
}

impl<'a> Writer<'a> {
    pub fn new(data: &'a mut Vec<u8>) -> Self {
        Self { data }
    }

    pub fn write_u8(&mut self, value: u8) {
        self.data.push(value);
    }

    pub fn write_u16(&mut self, value: u16) {
        self.data.extend_from_slice(&value.to_be_bytes());
    }

    pub fn write_u32(&mut self, value: u32) {
        self.data.extend_from_slice(&value.to_be_bytes());
    }

    pub fn write_bytes(&mut self, value: &[u8]) {
        let len = value.len();
        self.write_u16(len as u16);
        self.data.extend_from_slice(value);
    }

    pub fn write_array<const N: usize>(&mut self, value: &[u8; N]) {
        self.data.extend_from_slice(value);
    }

    pub fn write_string(&mut self, value: &str) {
        let len = value.len();
        self.write_u16(len as u16);
        self.data.extend_from_slice(value.as_bytes());
    }

    pub fn write_ip_addr(&mut self, ip: &IpAddr) {
        match ip {
            IpAddr::V4(ip) => {
                self.write_u8(4);
                self.data.extend_from_slice(&ip.octets());
            }
            IpAddr::V6(ip) => {
                self.write_u8(6);
                self.data.extend_from_slice(&ip.octets());
            }
        }
    }

    pub fn write_socket_addr(&mut self, addr: &SocketAddr) {
        self.write_ip_addr(&addr.ip());
        self.write_u16(addr.port());
    }
}
