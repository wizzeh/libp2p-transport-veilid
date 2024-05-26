mod address;
mod connection;
mod errors;
mod listener;
mod node_status;
mod settings;
mod stream;
mod stream_seq;
mod utils;

use futures::future::{Future, Ready};
use futures::FutureExt;

use futures::stream::StreamExt;
use libp2p::Multiaddr;
use node_status::NodeStatus;
use tokio_crate::runtime::Handle;
use tokio_crate::sync::mpsc::{channel, Receiver, Sender};

use libp2p::{
    core::{
        transport::{ListenerId, TransportError, TransportEvent},
        Transport,
    },
    identity::Keypair,
};

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};
use std::task::Poll;
use std::time::Duration;

use connection::VeilidConnection;
pub use errors::VeilidError;
use listener::VeilidListener;

use veilid_core::UpdateCallback;
use veilid_core::{ConfigCallback, VeilidUpdate};
pub use veilid_core::{VeilidAPI, VeilidConfig};
use wasm_timer::{Instant, Interval};

#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};

use crate::address::Address;
use crate::settings::lookup_config;
use crate::stream::VeilidStream;

mod proto {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/proto/generated/payload.rs"
    ));
}

struct Settings {
    veilid_network_message_limit_bytes: usize,
    transport_stream_packet_data_size_bytes: usize,
    connection_keepalive_timeout: u64,
    message_retry_timeout: u64,
    stream_timeout_secs: u64,
    heartbeat_secs: u64,
}

const SETTINGS: Settings = Settings {
    // veilid's network limit
    veilid_network_message_limit_bytes: 32768,
    // our limit to account for our header bytes
    transport_stream_packet_data_size_bytes: 32623,
    // when to retry sending an undelivered message
    message_retry_timeout: 5,
    // Send a status message, if no other message has been sent
    connection_keepalive_timeout: 5,
    // delete the stream if no message received by timeout
    stream_timeout_secs: 20,
    // background process frequency
    heartbeat_secs: 5,
};

#[derive(Debug, Clone)]
pub enum VeilidTransportType {
    /// VeilidTransport with direct connections (IP address not hidden)
    Unsafe,
    /// VeilidTransport with safe routes (IP address hidden)
    Safe,
}

pub struct VeilidTransport<VeilidConnection> {
    transport_type: VeilidTransportType,
    my_keypair: Arc<Keypair>,
    update_callback: Option<UpdateCallback>, // a callback passed at api_startup to receive veilid events
    config_callback: Option<ConfigCallback>, // a callback passed at api_startup to feed config values to veilid_core
    update_receiver: Option<Receiver<VeilidUpdate>>, // a stream of VeilidUpdate events from UpdateCallback
    api: Option<Arc<VeilidAPI>>, // This provides access to the main functionality of Veilid.
    runtime: Option<tokio_crate::runtime::Handle>, // handle to current runtime for thread spawning
    listener: Option<Arc<Mutex<VeilidListener>>>,
    events: VecDeque<
        TransportEvent<
            <VeilidTransport<VeilidConnection> as libp2p::core::Transport>::ListenerUpgrade,
            <VeilidTransport<VeilidConnection> as libp2p::core::Transport>::Error,
        >,
    >,
    status: Arc<RwLock<NodeStatus>>,
    heartbeat: Interval,
}

impl VeilidTransport<VeilidConnection> {
    pub fn new(
        handle: Option<Handle>,
        node_keys: Keypair,
        transport_type: VeilidTransportType,
    ) -> Self {
        fn create_update_callback(tx: Sender<VeilidUpdate>) -> UpdateCallback {
            Arc::new(move |update: VeilidUpdate| {
                trace!("update_callback | {:?}", update);

                let tx_clone = tx.clone();
                let update_clone = update.clone();

                tokio_crate::spawn(async move {
                    if let Err(error) = tx_clone.send(update_clone).await {
                        error!("update_callback | {:?}", error);
                    }
                });
            })
        }

        // Create the config callback to read from Settings
        let node_keys = Arc::new(node_keys);
        let keys_clone = node_keys.clone();
        let config_callback: ConfigCallback =
            Arc::new(move |config_key: String| lookup_config(&config_key, keys_clone.clone()));

        // Create the UpdateCallback
        let (tx, rx) = channel::<VeilidUpdate>(32);
        let update_callback = create_update_callback(tx);

        let heartbeat_duration = Duration::from_secs(SETTINGS.heartbeat_secs).clone();

        Self {
            transport_type,
            my_keypair: node_keys,
            update_callback: Some(update_callback),
            config_callback: Some(config_callback),
            api: None,
            update_receiver: Some(rx),
            runtime: handle,
            listener: None,
            events: VecDeque::new(),
            status: Arc::new(RwLock::new(NodeStatus::new())),
            heartbeat: Interval::new_at(
                Instant::now() + heartbeat_duration.clone(),
                heartbeat_duration.clone(),
            ),
        }
    }
}

impl<T> VeilidTransport<T> {
    pub fn pop_event(
        mut self: Pin<&mut Self>,
    ) -> Option<
        TransportEvent<
            <VeilidTransport<T> as libp2p::core::Transport>::ListenerUpgrade,
            <VeilidTransport<T> as libp2p::core::Transport>::Error,
        >,
    > {
        let this = self.as_mut().get_mut();
        this.events.pop_front()
    }
}

impl<T> Transport for VeilidTransport<T> {
    type Output = VeilidConnection;
    type Error = VeilidError;
    type Dial = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;
    type ListenerUpgrade = Ready<Result<Self::Output, Self::Error>>;

    fn listen_on(
        self: &mut VeilidTransport<T>,
        id: ListenerId,
        addr: Multiaddr,
    ) -> Result<(), TransportError<Self::Error>> {
        info!("VeilidTransport::listen_on id: {:?} addr: {:?}", id, addr);

        let update_cb = self
            .update_callback
            .as_ref()
            .cloned()
            .ok_or_else(|| TransportError::Other(VeilidError::CallbackNotDefined))?;

        let config_cb = self
            .config_callback
            .as_ref()
            .cloned()
            .ok_or_else(|| TransportError::Other(VeilidError::CallbackNotDefined))?;

        if self.api.is_none() {
            let (tx, rx) = std::sync::mpsc::channel();

            // Capture handle of an existing runtime
            let handle = self.runtime.as_ref().unwrap().clone();

            handle.spawn(async move {
                let result = veilid_core::api_startup(update_cb, config_cb).await;

                if let Ok(api_ref) = &result {
                    if let Err(err) = api_ref.attach().await {
                        tx.send(Err(err)).unwrap();
                        return;
                    }
                }

                tx.send(result).unwrap();
            });

            let api_result = rx.recv().unwrap();

            match api_result {
                Ok(api) => {
                    debug!("VeilidTransport::listen_on API {:?}", api);

                    self.api = Some(Arc::new(api.clone()));

                    let transport_type = self.transport_type.clone();

                    if let Some(update_receiver) = self.update_receiver.take() {
                        let listener = VeilidListener::new(
                            transport_type,
                            id,
                            update_receiver,
                            Arc::new(api),
                            self.status.clone(),
                            self.my_keypair.clone(),
                        );

                        self.listener = Some(Arc::new(Mutex::new(listener)));

                        return Ok(());
                    } else {
                        return Err(TransportError::Other(VeilidError::CouldNotCreateListener));
                    }
                }
                Err(e) => {
                    error!("VeilidTransport::listen_on Err {:?}", e);
                    return Err(TransportError::Other(errors::VeilidError::VeilidApiError(
                        e,
                    )));
                }
            }
        }

        Err(TransportError::Other(VeilidError::CouldNotCreateListener))
    }

    fn dial(
        self: &mut VeilidTransport<T>,
        addr: Multiaddr,
    ) -> Result<Self::Dial, TransportError<Self::Error>> {
        debug!("VeilidTransport: dial {:?}", addr);

        if let Ok(remote_address) = Address::try_from(addr.clone()) {
            debug!(
                "VeilidTransport: dial | remote_address {:?}",
                remote_address
            );

            let api = if let Some(api) = &self.api {
                api.clone()
            } else {
                return Err(TransportError::Other(VeilidError::APINotDefined));
            };

            if let Some(arc_listener) = self.listener.clone() {
                if let Ok(mut listener) = arc_listener.lock() {
                    let streams = listener.streams.clone();

                    let local_address = if let Some(address) = listener.get_local_address() {
                        address
                    } else {
                        error!("VeilidTransport: dial | Local Address not found");
                        return Err(TransportError::Other(VeilidError::Generic(
                            "Local address not found".to_string(),
                        )));
                    };

                    // check if we already have a pending connection
                    if let Some(stream) = streams.lock().unwrap().get(&remote_address) {
                        match *stream.status.lock().unwrap() {
                            stream::StreamStatus::Listen => {
                                debug!(
                                    "VeilidTransport: dial {:?} | stream listening | ignoring ",
                                    addr
                                );

                                return Err(TransportError::Other(VeilidError::Generic(
                                    String::from("Already connecting"),
                                )));
                            }
                            _ => {}
                        }
                    }

                    let keys_clone = self.my_keypair.clone();

                    let api = api.clone();
                    let listener_id = listener.id;

                    return Ok(async move {
                        if let Some(remote_target) = Address::to_target(&remote_address, &api).await
                        {
                            let stream = Arc::new(VeilidStream::new(
                                api,
                                listener_id,
                                local_address.clone(),
                                remote_address.clone(),
                                remote_target,
                                0,
                                keys_clone,
                            ));

                            streams
                                .lock()
                                .unwrap()
                                .insert(remote_address.clone(), stream.clone());

                            let mut connection =
                                VeilidConnection::new(local_address, remote_target, stream)?;

                            connection.connect().await;

                            info!(
                                "VeilidTransport | create_stream | remote_address {:?} | remote_target {:?}",
                                remote_address.to_key(),
                                remote_target
                            );

                            return Ok(connection);
                        } else {
                            error!("VeilidTransport: dial | Remote target not found");
                            return Err(VeilidError::Generic(
                                "remote target not found".to_string(),
                            ));
                        };
                    }
                    .boxed());
                } else {
                    return Err(TransportError::Other(VeilidError::Generic(
                        "Failed to lock listener".to_string(),
                    )));
                }
            } else {
                error!("VeilidTransport: dial | Listener not found");

                return Err(TransportError::Other(VeilidError::Generic(
                    "Listener not found".to_string(),
                )));
            }
        } else {
            error!("VeilidTransport: dial | Bad multiaddr");
            return Err(TransportError::MultiaddrNotSupported(addr));
        }
    }

    fn dial_as_listener(
        &mut self,
        addr: Multiaddr,
    ) -> Result<Self::Dial, TransportError<Self::Error>> {
        warn!("VeilidTransport: dial_as_listener {:?}", addr);

        self.dial(addr)
    }

    fn address_translation(&self, listen: &Multiaddr, observed: &Multiaddr) -> Option<Multiaddr> {
        warn!(
            "VeilidTransport: address_translation | listen: {:?} | observed: {:?}",
            listen, observed
        );
        None
    }

    fn remove_listener(&mut self, id: ListenerId) -> bool {
        info!("VeilidTransport: remove_listener {:?}", id);
        true
    }

    fn poll(
        mut self: Pin<&mut VeilidTransport<T>>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<libp2p::core::transport::TransportEvent<Self::ListenerUpgrade, Self::Error>>
    {
        if let Some(event) = self.as_mut().pop_event() {
            info!("VeilidTransport: poll {:?}", event);

            return Poll::Ready(event);
        }

        if let Some(arc_listener) = self.listener.clone() {
            if let Ok(mut listener) = arc_listener.lock() {
                match listener.get_next_event(cx) {
                    Poll::Ready(Some(event)) => {
                        info!("VeilidTransport | poll | listener {:?}", event);
                        return Poll::Ready(event);
                    }
                    Poll::Ready(None) => {
                        // The listener stream has ended
                        // return Poll::Ready(TransportEvent::ListenerClosed { listener_id: (), reason: () });
                    }
                    Poll::Pending => {
                        // The listener has no events available at the moment
                    }
                }
            }
        }

        // Trigger background maintenance process
        while let Poll::Ready(Some(())) = self.heartbeat.poll_next_unpin(cx) {
            heartbeat(&mut self);
        }

        Poll::Pending
    }
}

fn heartbeat<T>(transport: &mut Pin<&mut VeilidTransport<T>>) {
    trace!("VeilidTransport | heartbeat");

    let mut to_delete = Vec::new();

    // if let Some(api) = transport.api.clone() {
    //     tokio_crate::spawn(async move {
    //         match api.debug(String::from("peerinfo")).await {
    //             Ok(peerinfo) => debug!("VeilidTransport | heartbeat | peerinfo {:?}", peerinfo),
    //             Err(_) => {}
    //         }
    //     });
    // }

    if let Some(arc_listener) = transport.listener.clone() {
        if let Ok(listener) = arc_listener.lock() {
            let map = listener.streams.lock().unwrap();
            let streams = map.values().cloned();
            for stream in streams {
                if stream.is_active() {
                    stream
                        .send_status_if_stale()
                        .generate_messages()
                        .send_messages();
                } else if stream.is_expired() {
                    info!(
                        "VeilidTransport | heartbeat | stream has expired {:?}",
                        stream.remote_target
                    );
                    to_delete.push(stream.remote_address.clone());
                } else {
                    if let Ok(status) = stream.get_status() {
                        match status {
                            stream::StreamStatus::Dial => {
                                tokio_crate::spawn(async move {
                                    stream.send_dial().await;
                                });
                            }
                            stream::StreamStatus::Listen => {
                                tokio_crate::spawn(async move {
                                    stream.send_listen().await;
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    if let Some(arc_listener) = transport.listener.clone() {
        if let Ok(mut listener) = arc_listener.lock() {
            // check to see if the DHT record is recently active
            listener.check_address_dht_record();

            // send a ping to ourselves to test our private route
            let is_listener_attached = {
                if let Some(node_status) = listener.get_node_status() {
                    node_status.is_attached().clone()
                } else {
                    false
                }
            };

            let listener_has_target = {
                if let Ok(guard) = listener.local_target.lock() {
                    guard.0.is_some()
                } else {
                    false
                }
            };

            match transport.transport_type {
                VeilidTransportType::Unsafe => {}
                VeilidTransportType::Safe => {
                    if is_listener_attached && listener_has_target {
                        listener.send_ping();
                    }
                }
            }

            for remote_address in to_delete {
                let mut map = listener.streams.lock().unwrap();

                let stream = map.get(&remote_address);

                debug!(
                    "VeilidTransport | heartbeat | deleted stream {:?}",
                    remote_address.to_key()
                );

                if let Some(stream) = stream {
                    let stream = stream.clone();
                    map.remove(&remote_address);
                    // wake the stream so that libp2p detects the dead stream
                    stream.wake_to_read();
                }
            }

            if let Ok(guard) = listener.local_address.lock() {
                if let Some(ref local_address) = *guard {
                    let local_address = local_address.clone();
                    trace!(
                        "VeilidTransport | heartbeat | local_address {:?}",
                        local_address
                    );
                    if let Some(api) = transport.api.clone() {
                        tokio_crate::spawn(async move {
                            match Address::to_target(&local_address, &api).await {
                                Some(target) => {
                                    trace!(
                                        "VeilidTransport | heartbeat | found my own target {:?}",
                                        target
                                    );
                                }
                                None => {
                                    warn!(
                                        "VeilidTransport | heartbeat | DID NOT find my own target"
                                    );
                                }
                            }
                        });
                    }
                } else {
                    trace!("VeilidTransport | heartbeat | no local_address");
                }
            } else {
                error!("VeilidTransport | heartbeat | could not lock addresses");
            }

            // if this node's private route target requires updating
            if let Some(api) = transport.api.clone() {
                if listener.should_update_target() {
                    let api = api.clone();
                    listener.update_target(api);
                }
            }
        }
    }
}
