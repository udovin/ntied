use std::net::SocketAddr;

use crate::byteio::{Reader, Writer};
use crate::{Address, Error};

pub enum ServerRequest {
    Heartbeat,
    Register(ServerRegisterRequest),
    Connect(ServerConnectRequest),
}

impl ServerRequest {
    pub fn serialize(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut writer = Writer::new(&mut bytes);
        match self {
            ServerRequest::Heartbeat => {}
            ServerRequest::Register(v) => {
                writer.write_u8(1);
                writer.write_u32(v.request_id);
                writer.write_bytes(&v.public_key);
                writer.write_array(v.address.as_bytes());
            }
            ServerRequest::Connect(v) => {
                writer.write_u8(2);
                writer.write_u32(v.request_id);
                writer.write_array(v.address.as_bytes());
                writer.write_u32(v.source_id);
            }
        }
        bytes
    }

    pub fn deserialize(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.is_empty() {
            return Ok(Self::Heartbeat);
        }
        let mut reader = Reader::new(bytes);
        match reader.read_u8()? {
            1 => {
                let request_id = reader.read_u32()?;
                let public_key = reader.read_bytes()?;
                let address = Address::from_bytes(reader.read_array()?);
                Ok(Self::Register(ServerRegisterRequest {
                    request_id,
                    public_key,
                    address,
                }))
            }
            2 => {
                let request_id = reader.read_u32()?;
                let address = Address::from_bytes(reader.read_array()?);
                let source_id = reader.read_u32()?;
                Ok(Self::Connect(ServerConnectRequest {
                    request_id,
                    address,
                    source_id,
                }))
            }
            _ => Err("Unknown request type".into()),
        }
    }
}

pub struct ServerRegisterRequest {
    pub request_id: u32,
    pub public_key: Vec<u8>,
    pub address: Address,
}

pub struct ServerConnectRequest {
    pub request_id: u32,
    pub address: Address,
    pub source_id: u32,
}

pub enum ServerResponse {
    Heartbeat,
    Register(ServerRegisterResponse),
    RegisterError(ServerErrorResponse),
    Connect(ServerConnectResponse),
    ConnectError(ServerErrorResponse),
    IncomingConnection(ServerIncomingConnectionResponse),
}

impl ServerResponse {
    pub fn serialize(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut writer = Writer::new(&mut bytes);
        match self {
            Self::Heartbeat => {}
            Self::Register(v) => {
                writer.write_u8(1);
                writer.write_u32(v.request_id);
            }
            Self::RegisterError(v) => {
                writer.write_u8(2);
                writer.write_u32(v.request_id);
                writer.write_u16(v.code);
            }
            Self::Connect(v) => {
                writer.write_u8(3);
                writer.write_u32(v.request_id);
                writer.write_bytes(&v.public_key);
                writer.write_array(v.address.as_bytes());
                writer.write_socket_addr(&v.addr);
            }
            Self::ConnectError(v) => {
                writer.write_u8(4);
                writer.write_u32(v.request_id);
                writer.write_u16(v.code);
            }
            ServerResponse::IncomingConnection(response) => {
                writer.write_u8(5);
                writer.write_bytes(&response.public_key);
                writer.write_array(response.address.as_bytes());
                writer.write_socket_addr(&response.addr);
                writer.write_u32(response.source_id);
            }
        }
        bytes
    }

    pub fn deserialize(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.is_empty() {
            return Ok(Self::Heartbeat);
        }
        let mut reader = Reader::new(bytes);
        match reader.read_u8()? {
            1 => {
                let request_id = reader.read_u32()?;
                Ok(Self::Register(ServerRegisterResponse { request_id }))
            }
            2 => {
                let request_id = reader.read_u32()?;
                let code = reader.read_u16()?;
                Ok(Self::RegisterError(ServerErrorResponse {
                    request_id,
                    code,
                }))
            }
            3 => {
                let request_id = reader.read_u32()?;
                let public_key = reader.read_bytes()?;
                let address = Address::from_bytes(reader.read_array()?);
                let addr = reader.read_socket_addr()?;
                Ok(Self::Connect(ServerConnectResponse {
                    request_id,
                    public_key,
                    address,
                    addr,
                }))
            }
            4 => {
                let request_id = reader.read_u32()?;
                let code = reader.read_u16()?;
                Ok(Self::ConnectError(ServerErrorResponse { request_id, code }))
            }
            5 => {
                let public_key = reader.read_bytes()?;
                let address = Address::from_bytes(reader.read_array()?);
                let addr = reader.read_socket_addr()?;
                let source_id = reader.read_u32()?;
                Ok(Self::IncomingConnection(ServerIncomingConnectionResponse {
                    public_key,
                    address,
                    addr,
                    source_id,
                }))
            }
            _ => Err("Unknown response type".into()),
        }
    }
}

pub struct ServerRegisterResponse {
    pub request_id: u32,
}

pub struct ServerConnectResponse {
    pub request_id: u32,
    pub public_key: Vec<u8>,
    pub address: Address,
    pub addr: SocketAddr,
}

pub struct ServerErrorResponse {
    pub request_id: u32,
    pub code: u16,
}

pub struct ServerIncomingConnectionResponse {
    pub public_key: Vec<u8>,
    pub address: Address,
    pub addr: SocketAddr,
    pub source_id: u32,
}
