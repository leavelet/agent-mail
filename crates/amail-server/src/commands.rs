//! Commands for nodes: delivered as `command` events on the event streams
//! that node tokens hold open (`GET /v1/events`). A command names a seat;
//! the node hosting it acts, the others ignore it.

use amail_proto::NodeCommand;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

const CHANNEL: usize = 256;

#[derive(Default)]
pub struct NodeHub {
    orgs: Mutex<HashMap<i64, OrgNodes>>,
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
        (
            o.tx.subscribe(),
            NodeConn {
                hub: self.clone(),
                org_id,
            },
        )
    }

    /// Send a command to the org's connected nodes; returns how many.
    pub fn send(&self, org_id: i64, cmd: NodeCommand) -> usize {
        let orgs = self.orgs.lock().unwrap();
        match orgs.get(&org_id) {
            Some(o) if o.connected > 0 => {
                let _ = o.tx.send(cmd);
                o.connected
            }
            _ => 0,
        }
    }

    pub fn connected(&self, org_id: i64) -> usize {
        self.orgs
            .lock()
            .unwrap()
            .get(&org_id)
            .map_or(0, |o| o.connected)
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
        assert_eq!(hub.send(1, cmd), 0);
    }
}
