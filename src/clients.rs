//! Registry of connected websocket clients for the desktop window.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::Notify;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct VideoInfo {
    pub encoder: String,
    pub width: usize,
    pub height: usize,
    pub fps: f64,
}

#[derive(Serialize, Clone, Debug)]
pub struct ClientInfo {
    pub id: u64,
    pub ip: String,
    /// Unix time in milliseconds.
    pub connected_at_ms: u64,
    pub video: Option<VideoInfo>,
}

struct Entry {
    info: ClientInfo,
    disconnect: Arc<Notify>,
}

struct Inner {
    next_id: AtomicU64,
    entries: Mutex<BTreeMap<u64, Entry>>,
    on_change: Box<dyn Fn(Vec<ClientInfo>) + Send + Sync>,
}

#[derive(Clone)]
pub struct Clients(Arc<Inner>);

impl Clients {
    /// `on_change` runs while the registry lock is held, so snapshots can't arrive out of
    /// order; it must not block (the server passes a `try_send`).
    pub fn new(on_change: impl Fn(Vec<ClientInfo>) + Send + Sync + 'static) -> Self {
        Self(Arc::new(Inner {
            next_id: AtomicU64::new(1),
            entries: Mutex::new(BTreeMap::new()),
            on_change: Box::new(on_change),
        }))
    }

    pub fn add(&self, ip: IpAddr) -> ClientEntry {
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        let connected_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let disconnect = Arc::new(Notify::new());
        let mut entries = self.0.entries.lock().expect("lock poisoned");
        entries.insert(
            id,
            Entry {
                info: ClientInfo {
                    id,
                    ip: ip.to_canonical().to_string(),
                    connected_at_ms,
                    video: None,
                },
                disconnect: disconnect.clone(),
            },
        );
        self.notify(&entries);
        ClientEntry {
            clients: self.clone(),
            id,
            disconnect,
        }
    }

    pub fn snapshot(&self) -> Vec<ClientInfo> {
        Self::list(&self.0.entries.lock().expect("lock poisoned"))
    }

    /// Returns false for an unknown id.
    pub fn disconnect(&self, id: u64) -> bool {
        let entries = self.0.entries.lock().expect("lock poisoned");
        match entries.get(&id) {
            // stores a permit, so a client that isn't waiting yet is still stopped
            Some(entry) => {
                entry.disconnect.notify_one();
                true
            }
            None => false,
        }
    }

    /// The address of a connected client.
    pub fn ip_of(&self, id: u64) -> Option<String> {
        let entries = self.0.entries.lock().expect("lock poisoned");
        entries.get(&id).map(|entry| entry.info.ip.clone())
    }

    /// Disconnects every client connected from `ip`.
    pub fn disconnect_ip(&self, ip: IpAddr) {
        let ip = ip.to_canonical().to_string();
        let entries = self.0.entries.lock().expect("lock poisoned");
        for entry in entries.values().filter(|entry| entry.info.ip == ip) {
            entry.disconnect.notify_one();
        }
    }

    fn list(entries: &BTreeMap<u64, Entry>) -> Vec<ClientInfo> {
        entries.values().map(|entry| entry.info.clone()).collect()
    }

    fn notify(&self, entries: &BTreeMap<u64, Entry>) {
        (self.0.on_change)(Self::list(entries));
    }

    fn set_video(&self, id: u64, video: VideoInfo) {
        let mut entries = self.0.entries.lock().expect("lock poisoned");
        if let Some(entry) = entries.get_mut(&id) {
            entry.info.video = Some(video);
            self.notify(&entries);
        }
    }

    fn remove(&self, id: u64) {
        let mut entries = self.0.entries.lock().expect("lock poisoned");
        if entries.remove(&id).is_some() {
            self.notify(&entries);
        }
    }
}

/// A registered client; dropping it removes the client from the list.
pub struct ClientEntry {
    clients: Clients,
    id: u64,
    disconnect: Arc<Notify>,
}

impl ClientEntry {
    pub fn disconnect_signal(&self) -> Arc<Notify> {
        self.disconnect.clone()
    }

    pub fn video_reporter(&self) -> Box<dyn Fn(VideoInfo) + Send> {
        let clients = self.clients.clone();
        let id = self.id;
        Box::new(move |video| clients.set_video(id, video))
    }
}

impl Drop for ClientEntry {
    fn drop(&mut self) {
        self.clients.remove(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn counting() -> (Clients, Arc<Mutex<Vec<usize>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let clients = Clients::new(move |list| sink.lock().unwrap().push(list.len()));
        (clients, seen)
    }

    #[test]
    fn add_snapshot_and_drop() {
        let (clients, seen) = counting();
        let a = clients.add(ip("10.0.0.1"));
        let b = clients.add(ip("::ffff:10.0.0.2"));
        let list = clients.snapshot();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].ip, "10.0.0.1");
        assert_eq!(list[1].ip, "10.0.0.2");
        assert!(list[0].id < list[1].id);
        assert!(list[0].video.is_none());
        drop(a);
        assert_eq!(clients.snapshot().len(), 1);
        drop(b);
        assert!(clients.snapshot().is_empty());
        assert_eq!(*seen.lock().unwrap(), [1, 2, 1, 0]);
    }

    #[tokio::test]
    async fn disconnect_stores_a_permit() {
        let (clients, _) = counting();
        let entry = clients.add(ip("10.0.0.1"));
        let id = clients.snapshot()[0].id;
        assert!(clients.disconnect(id));
        let signal = entry.disconnect_signal();
        tokio::time::timeout(Duration::from_secs(1), signal.notified())
            .await
            .expect("signal was stored");
        assert!(!clients.disconnect(id + 100));
    }

    #[tokio::test]
    async fn disconnect_ip_signals_every_client_of_that_address() {
        let (clients, _) = counting();
        let a = clients.add(ip("10.0.0.1"));
        let b = clients.add(ip("::ffff:10.0.0.1"));
        let c = clients.add(ip("10.0.0.2"));
        clients.disconnect_ip(ip("10.0.0.1"));
        for entry in [&a, &b] {
            tokio::time::timeout(Duration::from_secs(1), entry.disconnect_signal().notified())
                .await
                .expect("signalled");
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(50), c.disconnect_signal().notified())
                .await
                .is_err()
        );
    }

    #[test]
    fn ip_of_unknown_id_is_none() {
        let (clients, _) = counting();
        let _a = clients.add(ip("10.0.0.1"));
        let id = clients.snapshot()[0].id;
        assert_eq!(clients.ip_of(id).as_deref(), Some("10.0.0.1"));
        assert_eq!(clients.ip_of(id + 1), None);
    }

    #[test]
    fn video_reporter_updates_the_snapshot() {
        let (clients, _) = counting();
        let entry = clients.add(ip("10.0.0.1"));
        let report = entry.video_reporter();
        let info = VideoInfo {
            encoder: "h264_vaapi".into(),
            width: 1920,
            height: 1080,
            fps: 60.0,
        };
        report(info.clone());
        assert_eq!(clients.snapshot()[0].video, Some(info.clone()));
        drop(entry);
        report(info); // removed client: no-op
        assert!(clients.snapshot().is_empty());
    }
}
