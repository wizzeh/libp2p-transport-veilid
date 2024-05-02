use futures::{AsyncRead, AsyncWrite};

use std::{
    fmt,
    io::{self, Error},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use veilid_core::Target;

use crate::{errors::VeilidError, stream::VeilidStream};

#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};

// Represents a connection between the local node and the remote node over the Veilid network.
/// Connections will be direct for now.
pub struct VeilidConnection {
    local_target: Target,
    remote_target: Target,
    stream: Arc<VeilidStream>,
}

impl VeilidConnection {
    pub fn new(
        local_target: Target,
        remote_target: Target,
        stream: Arc<VeilidStream>,
    ) -> Result<Self, VeilidError> {
        let connection = Self {
            local_target,
            remote_target: remote_target.clone(),
            stream,
        };

        Ok(connection)
    }

    pub async fn connect(&mut self) {
        info!("VeilidConnection | connect: {:?}", self);
        self.stream.send_dial().await;
    }
}

impl AsyncRead for VeilidConnection {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        debug!("AsyncRead for VeilidConnection | poll_read");

        if !self.stream.is_expired() {
            match self.stream.read_inbound_stream(cx, buf) {
                Some(readable) => Poll::Ready(Ok(readable)),
                None => {
                    *self.stream.waker.lock().unwrap() = Some(cx.waker().clone());
                    Poll::Pending
                }
            }
        } else {
            Poll::Ready(Err(Error::other("Stream is inactive")))
        }
    }
}

impl AsyncWrite for VeilidConnection {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        debug!(
            "AsyncWrite for VeilidConnection | poll_write | buf size {:?}",
            buf.len()
        );

        if !self.stream.is_expired() {
            let data = buf;
            let byte_count = data.len();

            debug!(
                "AsyncWrite for VeilidConnection | poll_write | try send {:?}",
                std::str::from_utf8(&data).unwrap_or("[Invalid UTF-8]")
            );

            self.stream.insert_to_outbound_stream(data);
            Poll::Ready(Ok(byte_count))
        } else {
            Poll::Ready(Err(Error::other("Stream is inactive")))
        }
    }

    // Not used
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // info!("AsyncWrite for VeilidConnection | poll_flush");
        Poll::Ready(Ok(()))
    }

    // Not used
    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // info!("AsyncWrite for VeilidConnection | poll_close: {:?}", self);
        Poll::Pending
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
