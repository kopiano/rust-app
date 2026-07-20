use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

use tokio::sync::{RwLock, mpsc};
use uuid::Uuid;

use crate::models::message::MessageBroadcast;

const DEFAULT_CONNECTION_QUEUE_CAPACITY: usize = 64;

type ConnectionSender = mpsc::Sender<Arc<str>>;
type UserConnections = HashMap<Uuid, ConnectionSender>;

pub struct MessageHub {
    connections: RwLock<HashMap<Uuid, UserConnections>>,
    connected: AtomicUsize,
    dropped_connections: AtomicU64,
    queue_capacity: usize,
}

impl Default for MessageHub {
    fn default() -> Self {
        Self {
            connections: RwLock::new(HashMap::new()),
            connected: AtomicUsize::new(0),
            dropped_connections: AtomicU64::new(0),
            queue_capacity: DEFAULT_CONNECTION_QUEUE_CAPACITY,
        }
    }
}

impl MessageHub {
    pub fn from_env() -> Self {
        Self {
            queue_capacity: env_usize(
                "WS_CONNECTION_QUEUE_CAPACITY",
                DEFAULT_CONNECTION_QUEUE_CAPACITY,
            ),
            ..Self::default()
        }
    }

    pub async fn register(&self, user_id: Uuid) -> (Uuid, mpsc::Receiver<Arc<str>>) {
        let connection_id = Uuid::new_v4();
        let (sender, receiver) = mpsc::channel(self.queue_capacity);
        self.connections
            .write()
            .await
            .entry(user_id)
            .or_default()
            .insert(connection_id, sender);
        self.connected.fetch_add(1, Ordering::Relaxed);
        (connection_id, receiver)
    }

    pub async fn unregister(&self, user_id: Uuid, connection_id: Uuid) {
        let mut connections = self.connections.write().await;
        let mut removed = false;
        let mut remove_user = false;
        if let Some(user_connections) = connections.get_mut(&user_id) {
            removed = user_connections.remove(&connection_id).is_some();
            remove_user = user_connections.is_empty();
        }
        if remove_user {
            connections.remove(&user_id);
        }
        if removed {
            self.connected.fetch_sub(1, Ordering::Relaxed);
        }
    }

    pub async fn dispatch(
        &self,
        event: &MessageBroadcast,
    ) -> Result<DeliveryReport, serde_json::Error> {
        let payload: Arc<str> = serde_json::to_string(event)?.into();
        let recipients = event.recipients.iter().copied().collect::<HashSet<_>>();
        let connections = self.connections.read().await;
        let mut stale_connections = Vec::new();
        let mut delivered = 0_u64;

        for user_id in recipients {
            let Some(user_connections) = connections.get(&user_id) else {
                continue;
            };
            for (connection_id, sender) in user_connections {
                match sender.try_send(payload.clone()) {
                    Ok(()) => delivered += 1,
                    Err(_) => stale_connections.push((user_id, *connection_id)),
                }
            }
        }
        drop(connections);

        let dropped = stale_connections.len() as u64;
        for (user_id, connection_id) in stale_connections {
            self.unregister(user_id, connection_id).await;
        }
        if dropped > 0 {
            self.dropped_connections
                .fetch_add(dropped, Ordering::Relaxed);
        }

        Ok(DeliveryReport { delivered, dropped })
    }

    pub fn connected(&self) -> usize {
        self.connected.load(Ordering::Relaxed)
    }

    pub fn dropped_connections(&self) -> u64 {
        self.dropped_connections.load(Ordering::Relaxed)
    }

    pub fn queue_capacity(&self) -> usize {
        self.queue_capacity
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DeliveryReport {
    pub delivered: u64,
    pub dropped: u64,
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::MessageHub;
    use crate::models::message::{Message, MessageBroadcast};
    use chrono::Utc;
    use std::{
        collections::HashMap,
        sync::atomic::{AtomicU64, AtomicUsize},
    };
    use tokio::sync::RwLock;
    use uuid::Uuid;

    fn event(recipients: Vec<Uuid>) -> MessageBroadcast {
        let sender_id = Uuid::new_v4();
        MessageBroadcast {
            event: "message",
            message: Message {
                id: 1,
                conversation_id: Uuid::new_v4(),
                chat_type: "private".to_owned(),
                send_id: sender_id,
                client_message_id: Some(Uuid::new_v4()),
                receiver_id: recipients.first().copied(),
                group_id: None,
                content: Some("hello".to_owned()),
                message_type: 1,
                status: "sent".to_owned(),
                created_at: Utc::now(),
                update_at: Utc::now(),
                deleted_at: None,
                file_name: None,
                file_url: None,
            },
            recipients,
        }
    }

    #[tokio::test]
    async fn dispatches_only_to_registered_recipients() {
        let hub = MessageHub::default();
        let recipient = Uuid::new_v4();
        let other = Uuid::new_v4();
        let (_, mut recipient_events) = hub.register(recipient).await;
        let (_, mut other_events) = hub.register(other).await;

        let report = hub.dispatch(&event(vec![recipient])).await.unwrap();

        assert_eq!(report.delivered, 1);
        assert!(recipient_events.recv().await.is_some());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), other_events.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn removes_connections_with_full_queues() {
        let hub = MessageHub {
            connections: RwLock::new(HashMap::new()),
            connected: AtomicUsize::new(0),
            dropped_connections: AtomicU64::new(0),
            queue_capacity: 1,
        };
        let recipient = Uuid::new_v4();
        let (_, _events) = hub.register(recipient).await;

        hub.dispatch(&event(vec![recipient])).await.unwrap();
        let report = hub.dispatch(&event(vec![recipient])).await.unwrap();

        assert_eq!(report.dropped, 1);
        assert_eq!(hub.connected(), 0);
        assert_eq!(hub.dropped_connections(), 1);
    }
}
