use futures::{AsyncRead, AsyncWrite};
use libp2p_core::Multiaddr;

use std::{
    fmt, io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use veilid_core::{
    CryptoKey,
    CryptoTyped,
    // Sequencing,
    Target,
    VeilidAPI,
};

use crate::{
    errors::VeilidError,
    streams::VeilidStream,
    utils::{
        cryptotyped_to_multiaddr, cryptotyped_to_target, get_my_node_id_from_veilid_state_config,
    },
};
use crate::{streams::VeilidStreamManager, utils::get_veilid_state_config};
// use crate::provider::{IfEvent, Incoming, Provider};
// use crate::veilid_listener::VeilidListener;

#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};

// Represents a connection between the local node and the remote node over the Veilid network.
/// Connections will be direct for now.
// #[derive(Clone)]
pub struct VeilidConnection {
    local_target: Target,
    remote_target: Target,
}

impl VeilidConnection {
    pub fn new(
        api: Arc<VeilidAPI>,
        local_target: Target,
        remote_target: Target,
    ) -> Result<Self, VeilidError> {
        let connection = Self {
            local_target,
            remote_target: remote_target.clone(),
        };

        VeilidStream::new(api, remote_target);
        Ok(connection)
    }

    // Send CONNECT to initiate the connection
    pub async fn connect(&self) {
        debug!("VeilidConnection | connect: {:?}", self);

        let target = &self.remote_target;

        if let Some(stream) = VeilidStreamManager::get_stream(target) {
            stream.insert_to_outbound_stream(b"CONNECT");
        } else {
            error!("VeilidConnection | connect | stream not found")
        }
    }

    pub fn upgrade(
        api: Arc<VeilidAPI>,
        sender: &CryptoTyped<CryptoKey>,
    ) -> (Self, Multiaddr, Multiaddr) {
        // After receiving CONNECT, create and upgrade a connection

        let veilid_state_config = get_veilid_state_config(Some(api.clone())).unwrap();
        let node_id = get_my_node_id_from_veilid_state_config(veilid_state_config).unwrap();

        let local_addr = cryptotyped_to_multiaddr(&node_id);
        let local_target = cryptotyped_to_target(&node_id);

        let remote_addr = cryptotyped_to_multiaddr(sender);
        let remote_target = cryptotyped_to_target(sender);

        let connection =
            VeilidConnection::new(api, local_target.clone(), remote_target.clone()).unwrap();

        (connection, local_addr, remote_addr)
    }

    pub async fn close(&self) -> Result<(), Box<dyn std::error::Error>> {
        // Logic to close the connection.
        todo!()
    }
}

impl AsyncRead for VeilidConnection {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let remote_target = &self.remote_target;
        let option = VeilidStreamManager::get_stream(remote_target);

        if let Some(stream) = option {
            match stream.read_inbound_stream(cx, buf) {
                Some(readable) => Poll::Ready(Ok(readable)),
                None => Poll::Pending,
            }
        } else {
            debug!(
                "AsyncRead for VeilidConnection | No stream found for target: {:?}",
                remote_target
            );
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::Other,
                "Stream not found",
            )));
        }
    }
}

impl AsyncWrite for VeilidConnection {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        trace!(
            "AsyncWrite for VeilidConnection | poll_write | buf size {:?}",
            buf.len()
        );

        let remote_target = &self.remote_target;
        let data = buf;
        let byte_count = data.len();

        trace!(
            "AsyncWrite for VeilidConnection | poll_write | try send {:?}",
            std::str::from_utf8(&data).unwrap_or("[Invalid UTF-8]")
        );

        let option = VeilidStreamManager::get_stream(&remote_target);
        if let Some(stream) = option {
            stream.insert_to_outbound_stream(data);
            Poll::Ready(Ok(byte_count))
        } else {
            debug!(
                "AsyncWrite for VeilidConnection | No stream found for target {:?}",
                remote_target
            );
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::Other,
                "Stream not found",
            )));
        }
    }

    // Not used
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        trace!("AsyncWrite for VeilidConnection | poll_flush");
        Poll::Ready(Ok(()))
    }

    // Not used
    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        debug!("AsyncWrite for VeilidConnection | poll_close: {:?}", self);
        Poll::Ready(Ok(()))
    }
}

impl fmt::Debug for VeilidConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VeilidConnection")
            .field("api", &"VeilidAPI".to_string())
            .field("local_target", &self.local_target)
            .field("remote_target", &self.remote_target)
            .finish()
    }
}
