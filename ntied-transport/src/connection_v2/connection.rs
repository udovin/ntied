use std::time::Instant;

pub struct Connection {}

pub enum Error {
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ConnectionId(u64);

#[derive(Clone, Copy, Debug)]
pub struct SendInfo {}

#[derive(Clone, Copy, Debug)]
pub struct RecvInfo {}

impl Connection {
    pub fn open(connection_id: ConnectionId) -> Self {
        Self {}
    }

    pub fn accept(connection_id: ConnectionId) -> Self {
        Self {}
    }

    pub fn recv(&mut self, buf: &mut [u8], info: RecvInfo) -> Result<usize, Error> {
        todo!()
    }

    pub fn send(&mut self, buf: &mut [u8]) -> Result<(usize, SendInfo), Error> {
        todo!()
    }

    // TODO: Replace with iterator.
    pub fn readable_streams(&self) -> Vec<u64> {
        todo!()
    }

    // TODO: Replace with iterator.
    pub fn writable_streams(&self) -> Vec<u64> {
        todo!()
    }

    pub fn stream_recv(&mut self, stream_id: u64, buf: &mut [u8]) -> Result<(usize, bool), Error> {
        todo!()
    }

    pub fn stream_send(
        &mut self,
        stream_id: u64,
        data: &[u8],
        close: bool,
    ) -> Result<usize, Error> {
        todo!()
    }

    // TODO: Replace with iterator.
    pub fn readable_channels(&self) -> Vec<u64> {
        todo!()
    }

    // TODO: Replace with iterator.
    pub fn writable_channels(&self) -> Vec<u64> {
        todo!()
    }

    pub fn channel_open(&mut self, channel_id: u64) -> Result<(), Error> {
        todo!()
    }

    pub fn channel_close(&mut self, channel_id: u64) -> Result<(), Error> {
        todo!()
    }

    pub fn channel_send(&mut self, channel_id: u64, data: &[u8]) -> Result<(), Error> {
        todo!()
    }

    pub fn channel_recv(&mut self, channel_id: u64) -> Result<(Vec<u8>, bool), Error> {
        todo!()
    }

    pub fn close(&mut self, err: u64, reason: &[u8]) -> Result<(), Error> {
        todo!()
    }

    pub fn is_established(&self) -> bool {
        todo!()
    }

    pub fn is_closed(&self) -> bool {
        todo!()
    }
}
