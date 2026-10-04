//! Commands for nodes: delivered as `command` events on the event streams
//! that node tokens hold open (`GET /v1/events`). A command names a seat;
//! the node hosting it acts, the others ignore it.
//!
//! With several server instances a node's stream may be open on another
//! instance: commands are then also forwarded to every instance (see
//! [`crate::cluster`]), and each instance publishes how many node streams
//! it holds per org, so that "is a node connected" has one answer.

use amail_proto::NodeCommand;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::{Notify, broadcast};

const CHANNEL: usize = 256;

type Forward = Box<dyn Fn(i64, &NodeCommand) + Send + Sync>;

#[derive(Default)]
pub struct NodeHub {
    orgs: Mutex<HashMap<i64, OrgNodes>>,
    /// Node streams per org open on other instances.
    remote: Mutex<HashMap<i64, i64>>,
    /// Signalled when the local stream counts change.
    pub changed: Notify,
    /// Sends a command to the other instances.
    forward: OnceLock<Forward>,
}

struct OrgNodes {
    tx: broadcast::Sender<NodeCommand>,
    /// Open node event streams.
    connected: usize,
}

impl OrgNodes {
    fn new() -> Self {
        OrgNodes {
            tx: broadcast::channel(CHANNEL).0,
            connected: 0,
        }
    }
}

/// Counts one open node stream; dropping it ends the count.
pub struct NodeConn {
    hub: Arc<NodeHub>,
    org_id: i64,
}

impl Drop for NodeConn {
    fn drop(&mut self) {
        let mut orgs = self.hub.orgs.lock().unwrap();
        if let Some(o) = orgs.get_mut(&self.org_id) {
            o.connected = o.connected.saturating_sub(1);
        }
        drop(orgs);
        self.hub.changed.notify_one();
    }
}

impl NodeHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// A node's event stream opened.
    pub fn connect(self: &Arc<Self>, org_id: i64) -> (broadcast::Receiver<NodeCommand>, NodeConn) {
        let mut orgs = self.orgs.lock().unwrap();
        let o = orgs.entry(org_id).or_insert_with(OrgNodes::new);
        o.connected += 1;
        let rx = o.tx.subscribe();
        drop(orgs);
        self.changed.notify_one();
        (
            rx,
            NodeConn {
                hub: self.clone(),
                org_id,
            },
        )
    }

    /// End the org's command streams on this instance.
    pub fn close_org(&self, org_id: i64) {
        self.orgs.lock().unwrap().remove(&org_id);
        self.changed.notify_one();
    }

    /// Forward commands to other instances with `f` (set once, by the
    /// cluster).
    pub fn set_forward(&self, f: Forward) {
        let _ = self.forward.set(f);
    }

    /// Deliver a command to this instance's node streams; returns how many.
    pub fn deliver(&self, org_id: i64, cmd: NodeCommand) -> usize {
        let orgs = self.orgs.lock().unwrap();
        match orgs.get(&org_id) {
            Some(o) if o.connected > 0 => {
                let _ = o.tx.send(cmd);
                o.connected
            }
            _ => 0,
        }
    }

    /// Send a command to the org's connected nodes, on every instance;
    /// returns how many there are.
    pub fn send(&self, org_id: i64, cmd: NodeCommand) -> usize {
        // Forwarded even when no remote stream is known yet: the counts of
        // other instances lag by a second or two.
        if let Some(f) = self.forward.get() {
            f(org_id, &cmd);
        }
        self.deliver(org_id, cmd) + self.remote(org_id)
    }

    fn remote(&self, org_id: i64) -> usize {
        self.remote
            .lock()
            .unwrap()
            .get(&org_id)
            .map_or(0, |n| (*n).max(0) as usize)
    }

    pub fn connected(&self, org_id: i64) -> usize {
        let local = self
            .orgs
            .lock()
            .unwrap()
            .get(&org_id)
            .map_or(0, |o| o.connected);
        local + self.remote(org_id)
    }

    /// Open node streams per org on this instance.
    pub fn local_counts(&self) -> Vec<(i64, i64)> {
        self.orgs
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, o)| o.connected > 0)
            .map(|(id, o)| (*id, o.connected as i64))
            .collect()
    }

    /// Replace what is known about the other instances' node streams.
    pub fn set_remote(&self, counts: HashMap<i64, i64>) {
        *self.remote.lock().unwrap() = counts;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_send_drop() {
        let hub = Arc::new(NodeHub::new());
        let cmd = NodeCommand {
            op: "wake".into(),
            seat: "a".into(),
            lease_ms: None,
            backfill: None,
            node: None,
        };
        assert_eq!(hub.send(1, cmd.clone()), 0);
        let (mut rx, conn) = hub.connect(1);
        assert_eq!(hub.connected(1), 1);
        assert_eq!(hub.send(1, cmd.clone()), 1);
        assert_eq!(rx.try_recv().unwrap(), cmd);
        assert_eq!(hub.connected(2), 0);
        drop(conn);
        assert_eq!(hub.connected(1), 0);
        assert_eq!(hub.send(1, cmd.clone()), 0);
        // Streams on another instance count, and commands are forwarded.
        let sent = Arc::new(Mutex::new(Vec::new()));
        let s2 = sent.clone();
        hub.set_forward(Box::new(move |org, c| {
            s2.lock().unwrap().push((org, c.clone()))
        }));
        hub.set_remote(HashMap::from([(1, 2)]));
        assert_eq!(hub.connected(1), 2);
        assert_eq!(hub.send(1, cmd.clone()), 2);
        assert_eq!(hub.send(2, cmd.clone()), 0);
        assert_eq!(sent.lock().unwrap().len(), 2);
    }
}
