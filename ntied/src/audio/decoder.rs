use std::sync::atomic::AtomicU64;
use std::sync::{Arc, atomic};

use tokio::sync::{Mutex as TokioMutex, mpsc};

use crate::packet::AudioDataPacket;

use super::AudioFrame;

pub struct Decoder {
    tx: mpsc::Sender<AudioDataPacket>,
    rx: TokioMutex<mpsc::Receiver<AudioFrame>>,
    sent_packets: Arc<AtomicU64>,
    received_frames: Arc<AtomicU64>,
    sent_bytes: Arc<AtomicU64>,
    received_bytes: Arc<AtomicU64>,
    task: tokio::task::JoinHandle<()>,
}

impl Decoder {
    const BUFFER_SIZE: usize = 100;

    pub fn new() -> Self {
        let (frame_tx, rx) = mpsc::channel(Self::BUFFER_SIZE);
        let (tx, packet_rx) = mpsc::channel(Self::BUFFER_SIZE);
        let rx = TokioMutex::new(rx);
        let sent_packets = Arc::new(AtomicU64::new(0));
        let received_frames = Arc::new(AtomicU64::new(0));
        let sent_bytes = Arc::new(AtomicU64::new(0));
        let received_bytes = Arc::new(AtomicU64::new(0));
        let task = tokio::spawn(Self::main_loop(
            frame_tx,
            packet_rx,
            sent_packets.clone(),
            received_frames.clone(),
            sent_bytes.clone(),
            received_bytes.clone(),
        ));
        Self {
            tx,
            rx,
            sent_packets,
            received_frames,
            sent_bytes,
            received_bytes,
            task,
        }
    }

    pub async fn send_packet(
        &self,
        packet: AudioDataPacket,
    ) -> Result<(), mpsc::error::SendError<AudioDataPacket>> {
        self.tx.send(packet).await
    }

    pub async fn recv_frame(&self) -> Option<AudioFrame> {
        self.rx.lock().await.recv().await
    }

    async fn main_loop(
        tx: mpsc::Sender<AudioFrame>,
        mut rx: mpsc::Receiver<AudioDataPacket>,
        sent_packets: Arc<AtomicU64>,
        received_frames: Arc<AtomicU64>,
        sent_bytes: Arc<AtomicU64>,
        received_bytes: Arc<AtomicU64>,
    ) {
        todo!()
    }

    pub fn stats(&self) -> DecoderStats {
        DecoderStats {
            sent_packets: self.sent_packets.load(atomic::Ordering::Relaxed),
            received_frames: self.received_frames.load(atomic::Ordering::Relaxed),
            sent_bytes: self.sent_bytes.load(atomic::Ordering::Relaxed),
            received_bytes: self.received_bytes.load(atomic::Ordering::Relaxed),
        }
    }
}

#[derive(Debug)]
pub struct DecoderStats {
    pub sent_packets: u64,
    pub received_frames: u64,
    pub sent_bytes: u64,
    pub received_bytes: u64,
}
