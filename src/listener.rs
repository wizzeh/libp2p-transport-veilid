use futures::future;
use futures::{future::Ready, Stream};

use libp2p_core::multiaddr::Protocol;
use libp2p_core::{transport::ListenerEvent, Multiaddr};
use std::borrow::Cow;
use std::str;
use std::sync::Arc;

use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio_crate::sync::{
    mpsc,
    mpsc::{Receiver, Sender},
};

use veilid_core::{Target, VeilidAPI, VeilidUpdate};

use crate::connections::VeilidConnection;
use crate::streams::{StreamStatus, VeilidStream, VeilidStreamManager};
use crate::utils::{
    cryptotyped_to_multiaddr, cryptotyped_to_target, get_my_node_id_from_veilid_state_config,
};
use crate::{errors::VeilidError, node_status::NodeStatus};

#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};

type VeilidListenerEvent<S> = ListenerEvent<Ready<Result<S, VeilidError>>, VeilidError>;

pub struct VeilidListener {
    rx: Receiver<VeilidUpdate>,
    async_tx: Sender<Poll<Option<Result<VeilidListenerEvent<VeilidConnection>, VeilidError>>>>,
    async_rx: Receiver<Poll<Option<Result<VeilidListenerEvent<VeilidConnection>, VeilidError>>>>,
    api: Option<Arc<VeilidAPI>>,
}

impl VeilidListener {
    pub fn new(rx: Receiver<VeilidUpdate>, api: Option<Arc<VeilidAPI>>) -> Self {
        let (async_tx, async_rx) = mpsc::channel(32); // 32 is the buffer size, adjust as needed

        Self {
            rx,
            async_tx,
            async_rx,
            api,
        }
    }
}

impl Stream for VeilidListener {
    type Item = Result<VeilidListenerEvent<VeilidConnection>, VeilidError>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        // Did we receive some work?
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(update)) => {
                match update {
                    VeilidUpdate::Network(_) => {}
                    VeilidUpdate::Log(_) => {}
                    VeilidUpdate::AppCall(_) => {}
                    _ => {
                        trace!("poll_next | Received update: {:?}", update);
                    }
                }

                // Clone the waker and other resources
                let waker = cx.waker().clone();
                // let api_clone = self.api.clone();
                let async_tx = self.async_tx.clone(); // Assume you've also cloned the Sender part of the async channel
                let api = self.api.clone();

                // Spawn a new task to handle the async logic
                tokio_crate::spawn(async move {
                    let result = convert_update(api, update).await;
                    async_tx
                        .send(result)
                        .await
                        .expect("Failed to send async result"); // Send back the result
                    waker.wake(); // Notify the original task to wake up and poll again
                });
            }
            Poll::Ready(None) => {
                warn!("poll_next | Stream has ended");
                return Poll::Ready(None);
            }
            Poll::Pending => {}
        }

        // Then, proceed to check to see if anything has returned from the updates channel
        if let Poll::Ready(Some(async_result)) = self.async_rx.poll_recv(cx) {
            return async_result;
        } else {
            return Poll::Pending;
        }
    }
}

async fn convert_update(
    api: Option<Arc<VeilidAPI>>,
    update: VeilidUpdate,
) -> Poll<Option<Result<VeilidListenerEvent<VeilidConnection>, VeilidError>>> {
    match update {
        VeilidUpdate::Log(_) => {
            debug!("VeilidUpdate | Log");
            Poll::Pending
        }

        VeilidUpdate::AppMessage(app_msg) => {
            trace!("VeilidUpdate | AppMessage : {:?}", app_msg);

            let api = api.unwrap();

            let sender = app_msg.sender().unwrap();
            let remote_addr = cryptotyped_to_multiaddr(sender);
            let remote_target = cryptotyped_to_target(sender);

            let (delivered_seq, seq, data) = match VeilidStream::decode_message(&app_msg.message())
            {
                Ok((r, s, d)) => (r, s, d),
                Err(e) => {
                    error!("VeilidUpdate | AppMessage {:?}", e);
                    return Poll::Pending;
                }
            };

            info!(
                "VeilidUpdate | AppMessage | they have {:?} | they sent {:?} | bytes {:?}",
                delivered_seq,
                seq,
                data.len()
            );

            trace!(
                "VeilidUpdate | AppMessage | data {:?}",
                String::from_utf8_lossy(&data)
            );

            //
            // CONNECT
            if String::from_utf8_lossy(&data).starts_with("\u{7}\0\0\0CONNECT") {
                info!("VeilidUpdate | AppMessage | Connect");

                // delete any stream
                if let Some(stream) = VeilidStreamManager::get_stream(&remote_target) {
                    VeilidStreamManager::remove_stream(stream);
                }

                let (connection, local_addr, remote_addr) = VeilidConnection::upgrade(api, &sender);

                let stream = VeilidStreamManager::get_stream(&remote_target).unwrap();

                // Remove "CONNECT" prefix from data
                let data_without_connect: Vec<u8> = if let Some(stripped) =
                    String::from_utf8(data.clone())
                        .unwrap()
                        .strip_prefix("\u{7}\0\0\0CONNECT")
                {
                    stripped.as_bytes().to_vec()
                } else {
                    data.clone()
                };

                let status = stream
                    .recv_message(delivered_seq, seq, data_without_connect)
                    .await;

                match status {
                    StreamStatus::Active => {
                        return Poll::Ready(Some(Ok(ListenerEvent::Upgrade {
                            upgrade: future::ok(connection),
                            local_addr,
                            remote_addr,
                        })));
                    }
                    StreamStatus::Expired => {
                        return Poll::Ready(Some(Ok(ListenerEvent::AddressExpired(remote_addr))))
                    }
                }
            }

            //
            // STATUS
            if String::from_utf8_lossy(&data).starts_with("STATUS") {
                trace!(
                    "VeilidUpdate | AppMessage | Status | they have {:?} they sent {:?}",
                    delivered_seq,
                    seq
                );

                if let Some(stream) = VeilidStreamManager::get_stream(&remote_target) {
                    stream
                        .update_outbound_delivered_seq(delivered_seq)
                        .update_inbound_last_timestamp_to_now();
                } else {
                    disconnect(api, remote_target).await;
                }

                return Poll::Pending;
            }
            //
            // DISCONNECT
            if String::from_utf8_lossy(&data).starts_with("DISCONNECT") {
                info!("VeilidUpdate | AppMessage | DISCONNECT");

                // delete any stream
                if let Some(stream) = VeilidStreamManager::get_stream(&remote_target) {
                    VeilidStreamManager::remove_stream(stream);
                }

                return Poll::Ready(Some(Ok(ListenerEvent::AddressExpired(remote_addr))));
            }
            //
            // All other payloads
            let option = VeilidStreamManager::get_stream(&remote_target);
            match option {
                None => {
                    error!("VeilidUpdate | AppMessage | stream not found | sending DISCONNECT");

                    disconnect(api, remote_target).await;

                    return Poll::Ready(Some(Ok(ListenerEvent::AddressExpired(remote_addr))));
                }
                Some(stream) => {
                    let status = stream.recv_message(delivered_seq, seq, data).await;
                    match status {
                        StreamStatus::Active => return Poll::Pending,
                        StreamStatus::Expired => {
                            return Poll::Ready(Some(Ok(ListenerEvent::AddressExpired(
                                remote_addr,
                            ))))
                        }
                    }
                }
            }
        }

        VeilidUpdate::AppCall(app_call) => {
            debug!("VeilidUpdate | AppCall : {:?}", app_call);
            return Poll::Pending;
        }

        VeilidUpdate::Attachment(attach_state) => {
            debug!("VeilidUpdate | Attachment {:?}", attach_state);

            let result = NodeStatus::get().await;

            match result {
                Ok(mut node_status) => {
                    // make it mutable for updates
                    if node_status.attach_state() != &Some(attach_state.state) {
                        node_status = node_status.update_attach_state(&attach_state.state);
                    }

                    let mut is_online = false;

                    if node_status.public_internet_ready() != &attach_state.public_internet_ready {
                        node_status = node_status
                            .update_public_internet_ready(&attach_state.public_internet_ready);

                        if attach_state.public_internet_ready == true {
                            is_online = true;
                        }
                    }

                    let _ = node_status.set().await;

                    if is_online {
                        let result = NodeStatus::get().await;
                        match result {
                            Ok(node_status) => {
                                if node_status.my_node_id().is_some() {
                                    let my_node_id = node_status.my_node_id().unwrap();
                                    let kind = my_node_id.kind;
                                    let value = my_node_id.value;

                                    let my_node_id = format!("{}:{}", kind, value);

                                    let my_node_id: Cow<str> = my_node_id.clone().into();

                                    let mut address: Multiaddr = Multiaddr::empty();

                                    address.push(Protocol::Unix(my_node_id));

                                    return Poll::Ready(Some(Ok(VeilidListenerEvent::NewAddress(
                                        address,
                                    ))));
                                } else {
                                    return Poll::Pending;
                                }
                            }
                            Err(_) => {
                                error!("Failed to fetch NodeStatus");
                                return Poll::Pending;
                            }
                        }
                    } else {
                        debug!("VeilidUpdate::Attachment | Need to add !is_online and remove the Swarm listener");
                        return Poll::Pending;
                    }
                }
                Err(e) => {
                    error!("VeilidUpdate::Config {:?}", e);
                    return Poll::Pending;
                }
            }
        }

        VeilidUpdate::Network(update) => {
            trace!("VeilidUpdate | Network {:?} ", update);
            Poll::Pending
        }

        VeilidUpdate::Config(veilid_state_config) => {
            trace!("VeilidUpdate: Config {:?}", &veilid_state_config);

            let my_node_id = if let Some(my_node_id) =
                get_my_node_id_from_veilid_state_config(*veilid_state_config)
            {
                my_node_id
            } else {
                return Poll::Pending;
            };

            let node_status = if let Ok(node_status) = NodeStatus::get().await {
                node_status
            } else {
                return Poll::Pending;
            };

            if node_status.my_node_id() != &Some(my_node_id) {
                let result = node_status.update_my_node_id(&my_node_id).set().await;

                match result {
                    Ok(_) => Poll::Pending,
                    Err(e) => {
                        error!("VeilidUpdate::Config {:?}", e);
                        Poll::Pending
                    }
                }
            } else {
                return Poll::Pending;
            }
        }

        VeilidUpdate::RouteChange(veilid_route_change) => {
            debug!("VeilidUpdate: RouteChange: {:?}", veilid_route_change);
            Poll::Pending
        }

        VeilidUpdate::ValueChange(value_change) => {
            debug!("VeilidUpdate | ValueChange {:?}", value_change);
            Poll::Pending
        }

        VeilidUpdate::Shutdown => {
            debug!("VeilidUpdate | Shutdown");
            Poll::Pending
        }
    }

    // NEXT STEP:
    // We are creating a Libp2p transport

    // VeilidConnectionEvents represents a stream of events related to connections between your veilid node and other nodes.

    // pub enum VeilidUpdate {
    //     Log(VeilidLog),
    //     AppMessage(VeilidAppMessage),
    //     AppCall(VeilidAppCall),
    //     Attachment(VeilidStateAttachment),
    //     Network(VeilidStateNetwork),
    //     Config(VeilidStateConfig),
    //     RouteChange(VeilidRouteChange),
    //     ValueChange(VeilidValueChange),
    //     Shutdown,
    // }

    // pub struct VeilidStateAttachment {
    //     pub state: AttachmentState,
    //     pub public_internet_ready: bool,
    //     pub local_network_ready: bool,
    // }

    // pub struct VeilidStateNetwork {
    //     pub started: bool,
    //     pub bps_down: ByteCount,
    //     pub bps_up: ByteCount,
    //     pub peers: Vec<PeerTableData>,
    // }

    // pub struct VeilidStateConfig {
    //     pub config: VeilidConfigInner,
    // }

    // pub struct VeilidRouteChange {
    //     pub dead_routes: Vec<RouteId>,
    //     pub dead_remote_routes: Vec<RouteId>,
    // }

    // type VeilidListenerEvent<S> = ListenerEvent<Ready<Result<S, VeilidError>>, VeilidError>;

    // pub enum ListenerEvent<TUpgr, TErr> {
    //     /// The transport is listening on a new additional [`Multiaddr`].
    //     NewAddress(Multiaddr),
    //     /// An upgrade, consisting of the upgrade future, the listener address and the remote address.
    //     Upgrade {
    //         /// The upgrade.
    //         upgrade: TUpgr,
    //         /// The local address which produced this upgrade.
    //         local_addr: Multiaddr,
    //         /// The remote address which produced this upgrade.
    //         remote_addr: Multiaddr,
    //     },
    //     /// A [`Multiaddr`] is no longer used for listening.
    //     AddressExpired(Multiaddr),
    //     /// A non-fatal error has happened on the listener.
    //     ///
    //     /// This event should be generated in order to notify the user that something wrong has
    //     /// happened. The listener, however, continues to run.
    //     Error(TErr),
    // }
}

pub async fn disconnect(api: Arc<VeilidAPI>, remote_target: Target) {
    let data = b"DISCONNECT".to_vec();
    let message_data = VeilidStream::encode_message(0, 0, data.into());
    let routing_context = api.routing_context();
    let _ = routing_context
        .app_message(remote_target, message_data)
        .await;
}
