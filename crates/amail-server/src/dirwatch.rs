//! Directory change notifications: `directory` events on `GET /v1/events`
//! for user tokens, sent when a seat or list of the org is created, changed
//! or deleted, so that the web interface follows `amail up` and the admin
//! pages without a reload. With several instances the change is also
//! forwarded to the others (see [`crate::cluster`]).

use amail_proto::DirectoryEvent;
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::broadcast;

const CHANNEL: usize = 256;

#[derive(Default)]
pub struct DirectoryHub {
    orgs: Mutex<HashMap<i64, broadcast::Sender<DirectoryEvent>>>,
}

impl DirectoryHub {
    /// Hand an event to this instance's subscribers of the org.
    /// The org's channel goes once its last subscriber has gone, so that
    /// the map holds only orgs with open event streams.
    pub fn deliver(&self, org_id: i64, ev: DirectoryEvent) {
        let mut orgs = self.orgs.lock().unwrap();
        if let Some(tx) = orgs.get(&org_id)
            && tx.send(ev).is_err()
        {
            orgs.remove(&org_id);
        }
    }

    /// Orgs with a channel (for tests).
    pub fn orgs(&self) -> usize {
        self.orgs.lock().unwrap().len()
    }

    pub fn subscribe(&self, org_id: i64) -> broadcast::Receiver<DirectoryEvent> {
        self.orgs
            .lock()
            .unwrap()
            .entry(org_id)
            .or_insert_with(|| broadcast::channel(CHANNEL).0)
            .subscribe()
    }

    /// End the org's subscriptions here.
    pub fn close_org(&self, org_id: i64) {
        self.orgs.lock().unwrap().remove(&org_id);
    }
}

/// A directory event.
pub fn event(kind: &str, name: &str, change: &str) -> DirectoryEvent {
    DirectoryEvent {
        kind: kind.to_string(),
        name: name.to_string(),
        change: change.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deliver_to_subscribers_of_the_org() {
        let hub = DirectoryHub::default();
        // Nobody listens yet: nothing is kept.
        hub.deliver(1, event("seat", "a", "created"));
        let mut rx = hub.subscribe(1);
        let mut other = hub.subscribe(2);
        hub.deliver(1, event("seat", "b", "deleted"));
        assert_eq!(rx.try_recv().unwrap(), event("seat", "b", "deleted"));
        assert!(other.try_recv().is_err());
        hub.close_org(1);
        assert!(matches!(
            rx.try_recv(),
            Err(broadcast::error::TryRecvError::Closed)
        ));
        // A channel without subscribers goes with the next event.
        drop(other);
        assert_eq!(hub.orgs(), 1);
        hub.deliver(2, event("seat", "c", "created"));
        assert_eq!(hub.orgs(), 0);
    }
}
