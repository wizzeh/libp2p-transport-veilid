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
use std::sync::{Arc, RwLock};
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

use crate::settings::lookup_config;
use crate::stream::VeilidStream;
use crate::utils::{
    cryptotyped_to_multiaddr, cryptotyped_to_target, get_my_node_id_from_veilid_state_config,
    get_veilid_state_config, multiaddr_to_target, validate_multiaddr_for_veilid,
};

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
    transport_stream_packet_data_size_bytes: 32760,
    // when to retry sending an undelivered message
    message_retry_timeout: 5,
    // Send a status message, if no other message has been sent
    connection_keepalive_timeout: 5,
    // delete the stream if no message received by timeout
    stream_timeout_secs: 15,
    // background process frequency
    heartbeat_secs: 5,
};

pub struct VeilidTransport<VeilidConnection> {
    my_keypair: Arc<Keypair>,
    update_callback: Option<UpdateCallback>, // a callback passed at api_startup to receive veilid events
    config_callback: Option<ConfigCallback>, // a callback passed at api_startup to feed config values to veilid_core
    update_receiver: Option<Receiver<VeilidUpdate>>, // a stream of VeilidUpdate events from UpdateCallback
    api: Option<Arc<VeilidAPI>>, // This provides access to the main functionality of Veilid.
    runtime: Option<tokio_crate::runtime::Handle>, // handle to current runtime for thread spawning
    listener: Option<VeilidListener>,
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
    pub fn new(handle: Option<Handle>, node_keys: Keypair) -> Self {
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
                    debug!("VeilidTransport::api_result API {:?}", api);

                    self.api = Some(Arc::new(api));
                }
                Err(e) => {
                    error!("VeilidTransport::api_result Err {:?}", e);
                    return Err(TransportError::Other(errors::VeilidError::VeilidApiError(
                        e,
                    )));
                }
            }
        }

        let api = self.api.clone();
        let veilid_state_config: veilid_core::VeilidStateConfig =
            get_veilid_state_config(api).unwrap();

        if let Some(node_id) = get_my_node_id_from_veilid_state_config(veilid_state_config) {
            let addr = cryptotyped_to_multiaddr(&node_id);

            info!(
                "VeilidTransport: VeilidTransport::listen_on address {}",
                addr.to_string()
            );

            if let Some(update_receiver) = self.update_receiver.take() {
                let listener = VeilidListener::new(
                    id,
                    update_receiver,
                    self.api.clone(),
                    self.status.clone(),
                    self.my_keypair.clone(),
                );
                self.listener = Some(listener);

                Ok(())
            } else {
                Err(TransportError::Other(VeilidError::CouldNotCreateListener))
            }
        } else {
            Err(TransportError::Other(VeilidError::CouldNotCreateListener))
        }
    }

    fn dial(
        self: &mut VeilidTransport<T>,
        addr: Multiaddr,
    ) -> Result<Self::Dial, TransportError<Self::Error>> {
        debug!("VeilidTransport: dial {:?}", addr);

        match validate_multiaddr_for_veilid(&addr) {
            Ok(_) => {}
            Err(e) => {
                error!("{}:{:?}", e, addr);
                return Err(TransportError::MultiaddrNotSupported(addr));
            }
        }

        let api = if let Some(api) = &self.api {
            api.clone()
        } else {
            return Err(TransportError::Other(VeilidError::APINotDefined));
        };

        let api_clone = api.clone();

        let veilid_state_config: veilid_core::VeilidStateConfig =
            get_veilid_state_config(Some(api_clone)).unwrap();
        let node_id = get_my_node_id_from_veilid_state_config(veilid_state_config).unwrap();

        let local_target = cryptotyped_to_target(&node_id);

        if let Some(listener) = &self.listener {
            let streams = listener.streams.clone();

            match multiaddr_to_target(&addr) {
                Ok(remote_target) => {
                    // check if we already have a pending connection

                    if let Some(stream) = streams.lock().unwrap().get(&remote_target) {
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

                    return Ok(async move {
                        let stream = Arc::new(VeilidStream::new(
                            api.clone(),
                            remote_target.clone(),
                            0,
                            keys_clone,
                        ));

                        streams
                            .lock()
                            .unwrap()
                            .insert(remote_target, stream.clone());

                        let mut connection =
                            VeilidConnection::new(local_target, remote_target, stream)?;

                        connection.connect().await;
                        return Ok(connection);
                    }
                    .boxed());
                }
                Err(_) => return Err(TransportError::MultiaddrNotSupported(addr)),
            }
        } else {
            return Err(TransportError::Other(VeilidError::Generic(
                "No listener on transport".to_string(),
            )));
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

        if let Some(listener) = &mut self.listener {
            match listener.poll_next_unpin(cx) {
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

    if let Some(listener) = &transport.listener {
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
                to_delete.push(stream.remote_target);
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

    if let Some(listener) = &transport.listener {
        for remote_target in to_delete {
            let mut map = listener.streams.lock().unwrap();

            let stream = map.get(&remote_target);

            debug!(
                "VeilidTransport | heartbeat | deleted stream {:?}",
                remote_target
            );

            if let Some(stream) = stream {
                let stream = stream.clone();
                map.remove(&remote_target);
                // wake the stream so that libp2p detects the dead stream
                stream.wake_to_read();
            }
        }
    }
}
