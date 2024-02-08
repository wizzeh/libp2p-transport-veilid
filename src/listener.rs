use futures::future;
use futures::{future::Ready, Stream};

use libp2p::core::multiaddr::Protocol;
use libp2p::core::transport::ListenerId;
use libp2p::core::{transport::TransportEvent, Multiaddr};
use std::borrow::Cow;
use std::collections::HashMap;
use std::str;
use std::sync::{Arc, Mutex, RwLock};

use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio_crate::sync::{
    mpsc,
    mpsc::{Receiver, Sender},
};

use veilid_core::{Target, VeilidAPI, VeilidUpdate};

use crate::connection::VeilidConnection;
use crate::stream::VeilidStream;
use crate::utils::{
    cryptotyped_to_multiaddr, cryptotyped_to_target, get_my_node_id_from_veilid_state_config,
    get_veilid_state_config,
};
use crate::{errors::VeilidError, node_status::NodeStatus};

#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};

type VeilidTransportEvent<S> = TransportEvent<Ready<Result<S, VeilidError>>, VeilidError>;

#[derive(Debug, PartialEq)]
enum ListenerStatus {
    Offline,
    Online,
}

type Streams = Arc<Mutex<HashMap<Target, Arc<VeilidStream>>>>;

#[derive(Debug)]
pub struct VeilidListener {
    pub id: ListenerId,
    rx: Receiver<VeilidUpdate>,
    async_tx: Sender<Poll<Option<VeilidTransportEvent<VeilidConnection>>>>,
    async_rx: Receiver<Poll<Option<VeilidTransportEvent<VeilidConnection>>>>,
    api: Option<Arc<VeilidAPI>>,
    node_status: Arc<RwLock<NodeStatus>>,
    status: Arc<RwLock<ListenerStatus>>,
    pub streams: Streams,
}

impl VeilidListener {
    pub fn new(
        id: ListenerId,
        rx: Receiver<VeilidUpdate>,
        api: Option<Arc<VeilidAPI>>,
        node_status: Arc<RwLock<NodeStatus>>,
    ) -> Self {
        let (async_tx, async_rx) = mpsc::channel(32);

        Self {
            id,
            rx,
            async_tx,
            async_rx,
            api,
            node_status,
            status: Arc::new(RwLock::new(ListenerStatus::Offline)),
            streams: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Stream for VeilidListener {
    type Item = TransportEvent<Ready<Result<VeilidConnection, VeilidError>>, VeilidError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        // Did we receive some work?
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(update)) => {
                match update {
                    VeilidUpdate::Network(ref network) => {
                        trace!("VeilidListener | Network {:?}", network);
                    }
                    VeilidUpdate::Log(_) => {}
                    VeilidUpdate::AppCall(_) => {}
                    _ => {
                        trace!("poll_next | Received update: {:?}", update);
                    }
                }

                // Clone the waker and other resources
                let waker = cx.waker().clone();
                // let api_clone = self.api.clone();
                let async_tx = self.async_tx.clone();
                let api = self.api.clone();
                let listener_id = self.id;

                // Spawn a new task to handle the async logic
                let node_status = self.node_status.clone();
                let listener_status = self.status.clone();

                let streams = self.streams.clone();

                tokio_crate::spawn(async move {
                    let result = convert_update(
                        streams,
                        api,
                        listener_id,
                        update,
                        node_status,
                        listener_status,
                    )
                    .await;
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
    streams: Arc<Mutex<HashMap<Target, Arc<VeilidStream>>>>,
    api: Option<Arc<VeilidAPI>>,
    listener_id: ListenerId,
    update: VeilidUpdate,
    node_status: Arc<RwLock<NodeStatus>>,
    listener_status: Arc<RwLock<ListenerStatus>>,
) -> Poll<Option<VeilidTransportEvent<VeilidConnection>>> {
    fn delete_stream(streams: Streams, remote_target: Target) {
        if let Ok(mut streams) = streams.lock() {
            streams.remove(&remote_target);
        }
    }

    match update {
        VeilidUpdate::Log(_) => {
            debug!("VeilidUpdate | Log");
            Poll::Pending
        }

        VeilidUpdate::AppMessage(app_msg) => {
            debug!("VeilidUpdate | AppMessage");
            debug!("VeilidUpdate | AppMessage : {:?}", app_msg);
            let mut should_disconnect = false;
            let mut should_delete = false;

            let api = api.unwrap();

            if let None = app_msg.sender() {
                error!("VeilidUpdate | AppMessage | No sender");
                return Poll::Pending;
            }

            let sender = app_msg.sender().unwrap();
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

            debug!(
                "VeilidUpdate | AppMessage | data {:?}",
                String::from_utf8_lossy(&data)
            );

            enum MessageType {
                Connect,
                Status,
                Disconnect,
                Data,
            }

            // Determine message type
            let message_type = if String::from_utf8_lossy(&data).starts_with("\u{7}\0\0\0CONNECT") {
                MessageType::Connect
            } else if String::from_utf8_lossy(&data).starts_with("STATUS") {
                MessageType::Status
            } else if String::from_utf8_lossy(&data).starts_with("DISCONNECT") {
                MessageType::Disconnect
            } else {
                MessageType::Data
            };

            match message_type {
                MessageType::Connect => {
                    info!("VeilidUpdate | AppMessage | Connect");

                    let veilid_state_config = get_veilid_state_config(Some(api.clone())).unwrap();
                    let node_id =
                        get_my_node_id_from_veilid_state_config(veilid_state_config).unwrap();

                    let local_addr = cryptotyped_to_multiaddr(&node_id);
                    let local_target = cryptotyped_to_target(&node_id);

                    let remote_addr = cryptotyped_to_multiaddr(sender);
                    let remote_target = cryptotyped_to_target(sender);

                    let stream = Arc::new(VeilidStream::new(api.clone(), remote_target.clone()));

                    streams
                        .lock()
                        .unwrap()
                        .insert(remote_target, stream.clone());

                    let connection = VeilidConnection::new(
                        api,
                        local_target.clone(),
                        remote_target.clone(),
                        stream.clone(),
                    )
                    .unwrap();

                    // Remove "CONNECT" prefix from data
                    let data_without_connect: Vec<u8> = match String::from_utf8(data.clone()) {
                        Ok(string) => {
                            if let Some(stripped) = string.strip_prefix("\u{7}\0\0\0CONNECT") {
                                stripped.as_bytes().to_vec()
                            } else {
                                string.as_bytes().to_vec() // If the prefix is not present, return the original string
                            }
                        }
                        Err(e) => {
                            error!("VeilidUpdate | AppMessage | Connect {:?}", e);
                            data // If there's an error, return the original data
                        }
                    };

                    stream.recv_message(delivered_seq, seq, data_without_connect);

                    return Poll::Ready(Some(TransportEvent::Incoming {
                        listener_id,
                        upgrade: future::ok(connection),
                        local_addr,
                        send_back_addr: remote_addr,
                    }));
                }
                MessageType::Status => {
                    info!(
                        "VeilidUpdate | AppMessage | Status | they have {:?} they sent {:?}",
                        delivered_seq, seq
                    );

                    if let Some(stream) = streams.lock().unwrap().get(&remote_target) {
                        if stream.is_active() {
                            stream
                                .update_outbound_delivered_seq(delivered_seq)
                                .update_inbound_last_timestamp_to_now();
                        } else {
                            debug!(
                                "VeilidUpdate | AppMessage | Status | stream inactive, sending disconnect and closing"
                            );
                            should_disconnect = true;
                            should_delete = true;
                        }
                    } else {
                        debug!(
                            "VeilidUpdate | AppMessage | Status | no stream, sending disconnect"
                        );
                        should_disconnect = true;
                    }
                }
                MessageType::Disconnect => {
                    info!("VeilidUpdate | AppMessage | DISCONNECT");
                    should_delete = true;
                }
                MessageType::Data => {
                    debug!("VeilidUpdate | AppMessage | data packet");
                    if let Some(stream) = streams.lock().unwrap().get(&remote_target) {
                        if stream.is_active() {
                            let stream = stream.clone();
                            stream.recv_message(delivered_seq, seq, data);
                        } else {
                            should_disconnect = true;
                            should_delete = true;
                        }
                    } else {
                        error!("VeilidUpdate | AppMessage | Other message | stream not found");
                        should_disconnect = true;
                    }
                }
            }

            if should_disconnect {
                disconnect(api, remote_target).await;
            }

            if should_delete {
                delete_stream(streams, remote_target);
            }

            return Poll::Pending;
        }

        VeilidUpdate::AppCall(app_call) => {
            debug!("VeilidUpdate | AppCall : {:?}", app_call);
            return Poll::Pending;
        }

        VeilidUpdate::Attachment(attach_state) => {
            info!("VeilidUpdate | Attachment {:?}", attach_state);

            let mut node_status = node_status.write().unwrap();

            let mut is_online = false;

            let is_attached = match attach_state.state {
                veilid_core::AttachmentState::Detached => false,
                veilid_core::AttachmentState::Attaching => false,
                veilid_core::AttachmentState::AttachedWeak => false,
                veilid_core::AttachmentState::AttachedGood => true,
                veilid_core::AttachmentState::AttachedStrong => true,
                veilid_core::AttachmentState::FullyAttached => true,
                veilid_core::AttachmentState::OverAttached => true,
                veilid_core::AttachmentState::Detaching => false,
            };

            if node_status.is_attached() != &is_attached {
                node_status.update_is_attached(&is_attached);
            }

            let public_internet_ready = attach_state.public_internet_ready;

            if node_status.public_internet_ready() != &public_internet_ready {
                node_status.update_public_internet_ready(&public_internet_ready);
            }

            let mut listener_status_guard = listener_status.write().unwrap();

            if *listener_status_guard == ListenerStatus::Offline {
                if public_internet_ready && is_attached {
                    is_online = true;
                    *listener_status_guard = ListenerStatus::Online;
                }
            }

            if is_online {
                if node_status.my_node_id().is_some() {
                    let my_node_id = node_status.my_node_id().unwrap();
                    let kind = my_node_id.kind;
                    let value = my_node_id.value;

                    let my_node_id = format!("{}:{}", kind, value);

                    let my_node_id: Cow<str> = my_node_id.clone().into();

                    let mut listen_addr: Multiaddr = Multiaddr::empty();

                    listen_addr.push(Protocol::Unix(my_node_id));

                    warn!("NodeStatus | online");

                    return Poll::Ready(Some(VeilidTransportEvent::NewAddress {
                        listener_id,
                        listen_addr,
                    }));
                } else {
                    return Poll::Pending;
                }
            } else {
                debug!("VeilidUpdate::Attachment | Need to add !is_online and remove the Swarm listener");
                return Poll::Pending;
            }
        }

        VeilidUpdate::Network(update) => {
            trace!("VeilidUpdate | Network {:?} ", update);
            Poll::Pending
        }

        VeilidUpdate::Config(veilid_state_config) => {
            info!("VeilidUpdate: Config {:?}", &veilid_state_config);
            let mut node_status = node_status.write().unwrap();

            let my_node_id = if let Some(my_node_id) =
                get_my_node_id_from_veilid_state_config(*veilid_state_config)
            {
                my_node_id
            } else {
                return Poll::Pending;
            };

            if node_status.my_node_id() != &Some(my_node_id) {
                node_status.update_my_node_id(&my_node_id);

                Poll::Pending
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
}

pub async fn disconnect(api: Arc<VeilidAPI>, remote_target: Target) {
    let data = b"DISCONNECT".to_vec();
    let message_data = VeilidStream::encode_message(0, 0, data.into());
    if let Ok(routing_context) = api.routing_context() {
        let _ = routing_context
            .with_safety(veilid_core::SafetySelection::Unsafe(
                veilid_core::Sequencing::NoPreference,
            ))
            .unwrap()
            .app_message(remote_target, message_data)
            .await;
    }
}
