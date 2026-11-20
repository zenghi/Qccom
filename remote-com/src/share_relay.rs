use std::{collections::HashMap, sync::Mutex};

use crate::sharing::{verify_relay_voucher, SHARE_RELAY_TOKEN};
use iroh_base::EndpointId;
use iroh_relay::server::{Access, AccessControl, ClientRequest, ConnectionId};

const MAX_CONNECTIONS: usize = 512;
const MAX_HOST_CLIENTS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    Host,
    Client { host: EndpointId },
}

#[derive(Debug)]
struct Active {
    endpoint: EndpointId,
    direction: Direction,
}

#[derive(Debug, Default)]
pub struct ShareAccess {
    active: Mutex<HashMap<ConnectionId, Active>>,
}

impl ShareAccess {
    fn admit(&self, endpoint: EndpointId, id: ConnectionId, token: Option<&str>) -> bool {
        let direction = match token {
            Some(SHARE_RELAY_TOKEN) => Direction::Host,
            Some(token) => match verify_relay_voucher(token) {
                Ok(voucher) if voucher.host() != endpoint => Direction::Client { host: voucher.host() },
                _ => return false,
            },
            None => return false,
        };

        let Ok(mut active) = self.active.lock() else {
            return false;
        };
        if active.len() >= MAX_CONNECTIONS {
            return false;
        }

        let existing = active.values().filter(|entry| entry.endpoint == endpoint);
        // The relay replaces an existing socket after admission, so permit temporary overlap.
        if existing.clone().count() >= 2 || existing.clone().any(|entry| entry.direction != direction) {
            return false;
        }
        if let Direction::Client { host } = direction {
            let hosting = active.values().any(|entry| entry.endpoint == host && entry.direction == Direction::Host);
            let joining = active.values().filter(|entry| entry.direction == direction).count();
            if !hosting || (joining >= MAX_HOST_CLIENTS && existing.count() == 0) {
                return false;
            }
        }

        active.insert(id, Active { endpoint, direction });
        true
    }
}

impl AccessControl for ShareAccess {
    async fn on_connect(&self, request: &ClientRequest) -> Access {
        if self.admit(request.endpoint_id(), request.connection_id(), request.auth_token().as_deref()) {
            Access::Allow
        } else {
            Access::Deny { reason: None }
        }
    }

    fn on_disconnect(&self, _endpoint: EndpointId, id: ConnectionId) {
        if let Ok(mut active) = self.active.lock() {
            active.remove(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sharing::{now_unix, ShareInvitation, ShareService, SHARE_LIFETIME_SECS};
    use iroh_base::SecretKey;
    use iroh_relay::server::OnDisconnectGuard;

    fn connection(endpoint: EndpointId) -> ConnectionId {
        OnDisconnectGuard::empty(endpoint).connection_id()
    }

    fn invite(host: &SecretKey) -> ShareInvitation {
        ShareInvitation::issue("https://localhost:7000".parse().unwrap(), host, ShareService::Com, 7001).unwrap()
    }

    #[test]
    fn requires_bearer_and_live_host() {
        let access = ShareAccess::default();
        let host = SecretKey::generate();
        let host_endpoint = host.public();
        let invite = invite(&host);
        let bearer = Some(invite.voucher().token());
        let client = SecretKey::generate().public();
        let client_connection = connection(client);

        assert!(!access.admit(client, client_connection, None));
        assert!(!access.admit(client, client_connection, Some("invalid")));
        assert!(!access.admit(client, client_connection, bearer));

        let first = connection(host_endpoint);
        let second = connection(host_endpoint);
        assert!(access.admit(host_endpoint, first, Some(SHARE_RELAY_TOKEN)));
        assert!(access.admit(host_endpoint, second, Some(SHARE_RELAY_TOKEN)));
        assert!(!access.admit(host_endpoint, connection(host_endpoint), Some(SHARE_RELAY_TOKEN)));
        assert!(!access.admit(host_endpoint, connection(host_endpoint), bearer));
        access.on_disconnect(host_endpoint, first);

        let expired = ShareInvitation::issue_at(invite.relay().clone(), &host, ShareService::Com, 7001, now_unix().unwrap() - SHARE_LIFETIME_SECS).unwrap();
        assert!(!access.admit(client, client_connection, Some(expired.voucher().token())));
        let mut forged = invite.voucher().token().to_owned();
        let position = forged.len() - 8;
        let changed = if forged.as_bytes()[position] == b'A' { "B" } else { "A" };
        forged.replace_range(position..position + 1, changed);
        assert!(!access.admit(client, client_connection, Some(&forged)));
        assert!(access.admit(client, client_connection, bearer));

        access.on_disconnect(host_endpoint, second);
        assert!(!access.admit(client, connection(client), bearer));
        access.on_disconnect(client, client_connection);
        assert!(access.active.lock().unwrap().is_empty());
    }

    #[test]
    fn bounds_connections_and_reopens_capacity() {
        let access = ShareAccess::default();
        let host = SecretKey::generate();
        let endpoint = host.public();
        let host_connection = connection(endpoint);
        assert!(access.admit(endpoint, host_connection, Some(SHARE_RELAY_TOKEN)));
        let invite = invite(&host);
        let bearer = Some(invite.voucher().token());
        let (first_client, first_connection) = {
            let client = SecretKey::generate().public();
            (client, connection(client))
        };
        assert!(access.admit(first_client, first_connection, bearer));
        for _ in 1..MAX_HOST_CLIENTS {
            let client = SecretKey::generate().public();
            assert!(access.admit(client, connection(client), bearer));
        }
        let extra_client = SecretKey::generate().public();
        let extra_connection = connection(extra_client);
        assert!(!access.admit(extra_client, extra_connection, bearer));
        access.on_disconnect(first_client, first_connection);
        assert!(access.admit(extra_client, extra_connection, bearer));

        for _ in (MAX_HOST_CLIENTS + 1)..MAX_CONNECTIONS {
            let host = SecretKey::generate().public();
            assert!(access.admit(host, connection(host), Some(SHARE_RELAY_TOKEN)));
        }
        let extra_host = SecretKey::generate().public();
        let extra_connection = connection(extra_host);
        assert!(!access.admit(extra_host, extra_connection, Some(SHARE_RELAY_TOKEN)));
        access.on_disconnect(endpoint, host_connection);
        assert!(access.admit(extra_host, extra_connection, Some(SHARE_RELAY_TOKEN)));
    }
}
