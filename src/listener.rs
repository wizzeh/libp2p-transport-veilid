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
use crate::stream::{StreamStatus, VeilidStream};
use crate::utils::{
    cryptotyped_to_target, get_my_node_id_from_veilid_state_config, get_veilid_state_config,
};
use crate::{errors::VeilidError, node_status::NodeStatus};

#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};

pub type VeilidTransportEvent<S> = TransportEvent<Ready<Result<S, VeilidError>>, VeilidError>;

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
    fn create_stream(
        api: &Arc<VeilidAPI>,
        remote_target: Target,
        remote_stream_id: u64,
        streams: &Streams,
    ) -> Arc<VeilidStream> {
        // create stream

        let stream = Arc::new(VeilidStream::new(
            api.clone(),
            remote_target.clone(),
            remote_stream_id,
        ));

        if let Ok(mut guard) = streams.lock() {
            guard.insert(remote_target, stream.clone());
        } else {
            error!("VeilidListener | create_stream | failed to lock streams");
        }
        stream
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
    match update {
        VeilidUpdate::Log(_) => {
            debug!("VeilidUpdate | Log");
            Poll::Pending
        }

        VeilidUpdate::AppMessage(app_msg) => {
            debug!("VeilidUpdate | AppMessage");
            trace!("VeilidUpdate | AppMessage : {:?}", app_msg);

            let api = api.unwrap();

            if let None = app_msg.sender() {
                error!("VeilidUpdate | AppMessage | No sender");
                return Poll::Pending;
            }

            let sender = app_msg.sender().unwrap();
            let remote_target = cryptotyped_to_target(sender);
            debug!(
                "VeilidUpdate | AppMessage | remote target {:?}",
                remote_target
            );

            trace!("VeilidUpdate | AppMessage | streams {:?}", streams);

            let option_stream = {
                let mutex_guard = streams.lock().unwrap();
                mutex_guard.get(&remote_target).map(Arc::clone)
            };

            let outbound_last_seq = match option_stream {
                Some(ref stream) => stream.get_outbound_last_seq(),
                None => 0,
            };

            let (delivered_seq, seq, stream_id, data) =
                match VeilidStream::decode_message(&app_msg.message()) {
                    Ok((r, s, i, d)) => (r, s, i, d),
                    Err(e) => {
                        error!("VeilidUpdate | AppMessage {:?}", e);
                        return Poll::Pending;
                    }
                };

            info!(
                "VeilidUpdate | AppMessage | they have {:?} of {:?} | they sent {:?} | bytes {:?} on stream_id {:?}",
                delivered_seq,
                outbound_last_seq,
                seq,
                data.len(),
                stream_id
            );

            trace!(
                "VeilidUpdate | AppMessage | data {:?}",
                String::from_utf8_lossy(&data)
            );

            enum MessageType {
                Dial,
                Listen,
                Status,
                Data,
            }

            // Determine message type
            let message_type = if String::from_utf8_lossy(&data).starts_with("DIAL") {
                MessageType::Dial
            } else if String::from_utf8_lossy(&data).starts_with("LISTEN") {
                MessageType::Listen
            } else if String::from_utf8_lossy(&data).starts_with("STATUS") {
                MessageType::Status
            } else {
                MessageType::Data
            };

            // refresh the stream if their stream_id is higher than previous
            if let Some(ref stream) = option_stream {
                if stream.get_remote_stream_id() == 0 {
                    debug!(
                        "VeilidUpdate | AppMessage | I dialed | now receiving their ID {:?}",
                        stream_id
                    );
                    stream.update_remote_stream_id(stream_id);
                } else {
                    stream.update_inbound_last_timestamp_to_now();
                }
            }

            match option_stream {
                None => {
                    match message_type {
                        MessageType::Dial => {
                            // Happy Path
                            debug!("VeilidUpdate | AppMessage | Stream None | received DIAL");

                            let remote_stream_id = stream_id;

                            let stream = VeilidListener::create_stream(
                                &api,
                                remote_target,
                                remote_stream_id,
                                &streams,
                            );

                            stream
                                .update_remote_stream_id(stream_id)
                                .update_status(StreamStatus::Listen)
                                .send_listen()
                                .await;

                            return stream.activate(sender, remote_target, listener_id);
                        }
                        MessageType::Listen => {
                            debug!("VeilidUpdate | AppMessage | Stream None | received LISTEN | ignoring");
                        }

                        MessageType::Status => {
                            debug!("VeilidUpdate | AppMessage | Stream None | received STATUS | ignoring");
                        }
                        MessageType::Data => {
                            debug!("VeilidUpdate | AppMessage | Stream None | received DATA | ignoring");
                        }
                    }
                }
                Some(ref stream) => {
                    debug!(
                        "VeilidUpdate | AppMessage | they sent {:?} and my remote_stream_id is {:?}",
                        stream_id, stream.get_remote_stream_id()
                    );

                    if stream.get_remote_stream_id() != 0
                        && stream_id < stream.get_remote_stream_id()
                    {
                        debug!("VeilidUpdate | AppMessage | msg from previous stream | ignoring");
                        return Poll::Pending;
                    }

                    let option_status = match stream.status.lock() {
                        Err(e) => {
                            error!(
                                "VeilidUpdate | AppMessage | couldnt lock stream status {:?}",
                                e
                            );
                            None
                        }
                        Ok(status) => Some(status.clone()),
                    };

                    if let Some(status) = option_status {
                        match status {
                            StreamStatus::Dial => {
                                match message_type {
                                    MessageType::Dial => {
                                        debug!(
                                            "VeilidUpdate | AppMessage | Stream DIAL | received DIAL"
                                        );
                                        let veilid_state_config =
                                            get_veilid_state_config(Some(api.clone())).unwrap();
                                        let node_id = get_my_node_id_from_veilid_state_config(
                                            veilid_state_config,
                                        )
                                        .unwrap();

                                        let local_target = cryptotyped_to_target(&node_id);

                                        // Need to choose role based on ID
                                        match remote_target.cmp(&local_target) {
                                            std::cmp::Ordering::Less => {
                                                debug!(
                                                    "VeilidUpdate | AppMessage | Stream DIAL | received DIAL | I'm the dialer"
                                                );
                                                stream
                                                    .update_inbound_last_timestamp_to_now()
                                                    .update_status(StreamStatus::Dial)
                                                    .send_dial()
                                                    .await;
                                            }
                                            std::cmp::Ordering::Equal => {
                                                warn!(
                                                    "VeilidUpdate | AppMessage | Stream DIAL | received DIAL | can't choose roles"
                                                );
                                                return Poll::Ready(None);
                                            }
                                            std::cmp::Ordering::Greater => {
                                                debug!(
                                                    "VeilidUpdate | AppMessage | Stream DIAL | received DIAL | I'm the listener"
                                                );

                                                stream
                                                    .update_remote_stream_id(stream_id)
                                                    .update_status(StreamStatus::Listen)
                                                    .send_listen()
                                                    .await;

                                                return stream.activate(
                                                    sender,
                                                    remote_target,
                                                    listener_id,
                                                );
                                            }
                                        };
                                    }
                                    MessageType::Listen => {
                                        // Happy Path
                                        debug!("VeilidUpdate | AppMessage | Stream DIAL | received LISTEN");

                                        stream
                                            .update_inbound_last_timestamp_to_now()
                                            .update_remote_stream_id(stream_id)
                                            .update_status(StreamStatus::Active)
                                            .generate_messages()
                                            .send_messages();
                                    }

                                    MessageType::Status => {
                                        debug!(
                                            "VeilidUpdate | AppMessage | Stream DIAL | received STATUS | ignoring"
                                        );
                                    }
                                    MessageType::Data => {
                                        debug!("VeilidUpdate | AppMessage | Stream DIAL | received DATA | ignoring");
                                    }
                                }
                            }
                            StreamStatus::Listen => {
                                match message_type {
                                    MessageType::Dial => {
                                        debug!("VeilidUpdate | AppMessage | Stream LISTEN | received DIAL");

                                        stream
                                            .update_remote_stream_id(stream_id)
                                            .send_listen()
                                            .await;
                                    }
                                    MessageType::Listen => {
                                        debug!("VeilidUpdate | AppMessage | Stream LISTEN | received LISTEN | ignoring");
                                    }

                                    MessageType::Status => {
                                        debug!(
                                            "VeilidUpdate | AppMessage | Stream LISTEN | received STATUS | ignoring"
                                        );
                                    }
                                    MessageType::Data => {
                                        // Happy Path
                                        debug!("VeilidUpdate | AppMessage | Stream LISTEN | received DATA");

                                        if stream.get_remote_stream_id() == stream_id {
                                            debug!("VeilidUpdate | AppMessage | Stream LISTEN | received DATA | matching stream_id");

                                            stream
                                                .update_status(StreamStatus::Active)
                                                .recv_message(delivered_seq, seq, data);
                                        }
                                    }
                                }
                            }

                            StreamStatus::Active => match message_type {
                                MessageType::Dial => {
                                    if stream.get_remote_stream_id() < stream_id {
                                        debug!(
                                            "VeilidUpdate | AppMessage | Stream ACTIVE | received DIAL | New stream"
                                        );
                                        let remote_stream_id = stream_id;

                                        let stream = VeilidListener::create_stream(
                                            &api,
                                            remote_target,
                                            remote_stream_id,
                                            &streams,
                                        );

                                        stream
                                            .update_remote_stream_id(stream_id)
                                            .update_status(StreamStatus::Listen)
                                            .send_listen()
                                            .await;

                                        return stream.activate(sender, remote_target, listener_id);
                                    } else {
                                        debug!(
                                            "VeilidUpdate | AppMessage | Stream ACTIVE | received DIAL | ignoring"
                                        );
                                    }
                                }
                                MessageType::Listen => {
                                    debug!(
                                            "VeilidUpdate | AppMessage | Stream ACTIVE | received LISTEN | ignoring"
                                        );
                                }
                                MessageType::Status => {
                                    debug!("VeilidUpdate | AppMessage | Stream ACTIVE | received STATUS");

                                    stream
                                        .update_outbound_delivered_seq(delivered_seq)
                                        .remove_sent_messages_from_queue();
                                }
                                MessageType::Data => {
                                    debug!(
                                        "VeilidUpdate | AppMessage | Stream ACTIVE | received DATA"
                                    );
                                    stream.recv_message(delivered_seq, seq, data);
                                }
                            },
                            StreamStatus::Expired => match message_type {
                                MessageType::Dial => {
                                    debug!(
                                            "VeilidUpdate | AppMessage | Stream EXPIRED | received DIAL | ignoring"
                                        )
                                }
                                MessageType::Listen => {
                                    debug!("VeilidUpdate | AppMessage | Stream EXPIRED | received LISTEN | ignoring")
                                }

                                MessageType::Status => {
                                    debug!("VeilidUpdate | AppMessage | Stream EXPIRED | received STATUS | ignoring")
                                }
                                MessageType::Data => {
                                    debug!("VeilidUpdate | AppMessage | Stream EXPIRED | received DATA | ignoring")
                                }
                            },
                        }
                    }
                }
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

                    info!("NodeStatus | online");

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
