use std::sync::atomic::AtomicU64;
use std::sync::{Arc, atomic};

use tokio::sync::{Mutex as TokioMutex, mpsc};

use crate::packet::AudioDataPacket;

use super::AudioFrame;

pub struct Encoder {
    tx: mpsc::Sender<AudioFrame>,
    rx: TokioMutex<mpsc::Receiver<AudioDataPacket>>,
    sent_frames: Arc<AtomicU64>,
    received_packets: Arc<AtomicU64>,
    sent_bytes: Arc<AtomicU64>,
    received_bytes: Arc<AtomicU64>,
    task: tokio::task::JoinHandle<()>,
}

impl Encoder {
    const BUFFER_SIZE: usize = 100;

    pub fn new() -> Self {
        let (tx, frame_rx) = mpsc::channel(Self::BUFFER_SIZE);
        let (packet_tx, rx) = mpsc::channel(Self::BUFFER_SIZE);
        let rx = TokioMutex::new(rx);
        let sent_frames = Arc::new(AtomicU64::new(0));
        let received_packets = Arc::new(AtomicU64::new(0));
        let sent_bytes = Arc::new(AtomicU64::new(0));
        let received_bytes = Arc::new(AtomicU64::new(0));
        let task = tokio::spawn(Self::main_loop(
            packet_tx,
            frame_rx,
            sent_frames.clone(),
            received_packets.clone(),
            sent_bytes.clone(),
            received_bytes.clone(),
        ));
        Self {
            tx,
            rx,
            sent_frames,
            received_packets,
            sent_bytes,
            received_bytes,
            task,
        }
    }

    pub async fn send_frame(
        &self,
        frame: AudioFrame,
    ) -> Result<(), mpsc::error::SendError<AudioFrame>> {
        self.tx.send(frame).await
    }

    pub async fn recv_packet(&self) -> Option<AudioDataPacket> {
        self.rx.lock().await.recv().await
    }

    async fn main_loop(
        tx: mpsc::Sender<AudioDataPacket>,
        mut rx: mpsc::Receiver<AudioFrame>,
        sent_frames: Arc<AtomicU64>,
        received_packets: Arc<AtomicU64>,
        sent_bytes: Arc<AtomicU64>,
        received_bytes: Arc<AtomicU64>,
    ) {
        todo!()
    }

    pub fn stats(&self) -> EncoderStats {
        EncoderStats {
            sent_frames: self.sent_frames.load(atomic::Ordering::Relaxed),
            received_packets: self.received_packets.load(atomic::Ordering::Relaxed),
            sent_bytes: self.sent_bytes.load(atomic::Ordering::Relaxed),
            received_bytes: self.received_bytes.load(atomic::Ordering::Relaxed),
        }
    }
}

#[derive(Debug)]
pub struct EncoderStats {
    pub sent_frames: u64,
    pub received_packets: u64,
    pub sent_bytes: u64,
    pub received_bytes: u64,
}
