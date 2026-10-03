use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

pub type Chunk = Arc<Vec<u8>>;

pub struct Hub {
    clients: Mutex<Vec<SyncSender<Chunk>>>,
    headers: Mutex<Arc<Vec<u8>>>,
    ready: AtomicBool,
}

impl Hub {
    pub fn new() -> Self {
        Hub {
            clients: Mutex::new(Vec::new()),
            headers: Mutex::new(Arc::new(Vec::new())),
            ready: AtomicBool::new(false),
        }
    }

    /// First call publishes the container header. A later call is a new encoder
    /// session: current listeners are dropped so they reconnect and resync.
    pub fn set_headers(&self, headers: Arc<Vec<u8>>) {
        let mut slot = self.headers.lock().unwrap_or_else(|e| e.into_inner());
        let restart = self.ready.swap(true, Ordering::AcqRel);
        *slot = headers;
        drop(slot);
        if restart {
            self.lock().clear();
        }
    }

    /// Replace the header new listeners will receive. Does not disconnect anyone.
    pub fn update_headers(&self, headers: Arc<Vec<u8>>) {
        *self.headers.lock().unwrap_or_else(|e| e.into_inner()) = headers;
        self.ready.store(true, Ordering::Release);
    }

    pub fn ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub fn headers(&self) -> Arc<Vec<u8>> {
        self.headers.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn subscribe(&self) -> Receiver<Chunk> {
        let (tx, rx) = sync_channel(256);
        self.lock().push(tx);
        rx
    }

    pub fn broadcast(&self, chunk: Chunk) {
        let mut clients = self.lock();
        clients.retain(|tx| match tx.try_send(chunk.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => false,
            Err(TrySendError::Disconnected(_)) => false,
        });
    }

    pub fn count(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<SyncSender<Chunk>>> {
        self.clients.lock().unwrap_or_else(|e| e.into_inner())
    }
}
