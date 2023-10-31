mod connection;
mod errors;
mod listener;
mod node_status;
mod settings;
mod stream;
mod stream_handler;
mod stream_seq;
mod utils;

use futures::future::{Future, Ready};
use futures::FutureExt;

use tokio_crate::runtime::Handle;
use tokio_crate::sync::mpsc::{channel, Receiver, Sender};

use libp2p_core::Transport;
use libp2p_core::{transport::TransportError, Multiaddr};

use std::pin::Pin;
use std::sync::Arc;

use connection::VeilidConnection;
pub use errors::VeilidError;
use listener::VeilidListener;

use veilid_core::UpdateCallback;
use veilid_core::{ConfigCallback, VeilidUpdate};
pub use veilid_core::{VeilidAPI, VeilidConfig};

#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};

use crate::settings::lookup_config;
use crate::stream_handler::StreamHandler;
use crate::utils::{
    cryptotyped_to_target, get_my_node_id_from_veilid_state_config, get_veilid_state_config,
    multiaddr_to_target, validate_multiaddr_for_veilid,
};

struct Settings {
    veilid_network_message_limit_bytes: usize,
    transport_stream_packet_data_size_bytes: usize,
    connection_keepalive_timeout: u64,
    message_retry_timeout: u64,
    stream_timeout_secs: u64,
}

const SETTINGS: Settings = Settings {
    // veilid's network limit
    veilid_network_message_limit_bytes: 32768,
    // our limit to account for our header bytes
    transport_stream_packet_data_size_bytes: 32760,
    // when to retry sending an undelivered message
    message_retry_timeout: 5,
    // Send a status message, if the node hasn't sent another message
    connection_keepalive_timeout: 5,
    // delete the stream if no message received by timeout
    stream_timeout_secs: 15,
};

pub struct VeilidTransport<T> {
    _impl: std::marker::PhantomData<T>,
    update_callback: Option<UpdateCallback>, // a callback passed at api_startup to receive veilid events
    config_callback: Option<ConfigCallback>, // a callback passed at api_startup to feed config values to veilid_core
    update_receiver: Option<Receiver<VeilidUpdate>>, // a stream of VeilidUpdate events from UpdateCallback
    api: Option<Arc<VeilidAPI>>, // This provides access to the main functionality of Veilid.
    runtime: Option<tokio_crate::runtime::Handle>, // handle to current runtime for thread spawning
}

impl VeilidTransport<VeilidConnection> {
    pub fn new(handle: Option<Handle>) -> Self {
        fn create_update_callback(tx: Sender<VeilidUpdate>) -> UpdateCallback {
            let tx = tx.clone();
            Arc::new(move |update: VeilidUpdate| {
                trace!("update_callback | {:?}", update);

                let tx_clone = tx.clone();
                let update_clone = update.clone(); // clone it here

                tokio_crate::spawn(async move {
                    tx_clone
                        .send(update_clone)
                        .await
                        .expect("Channel send failed");
                });
            })
        }

        // Create the config callback to read from Settings
        let config_callback: ConfigCallback =
            Arc::new(move |config_key: String| lookup_config(&config_key));

        // Create the UpdateCallback
        let (tx, rx) = channel::<VeilidUpdate>(32);
        let update_callback = create_update_callback(tx);

        Self {
            update_callback: Some(update_callback),
            config_callback: Some(config_callback),
            api: None,
            update_receiver: Some(rx),
            _impl: std::marker::PhantomData,
            runtime: handle,
        }
    }
}

impl Default for VeilidTransport<VeilidConnection> {
    fn default() -> Self {
        Self::new(None)
    }
}

impl<T> Transport for VeilidTransport<T> {
    type Output = VeilidConnection;
    type Error = VeilidError;
    type Dial = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;
    type Listener = VeilidListener;
    type ListenerUpgrade = Ready<Result<Self::Output, Self::Error>>;

    fn listen_on(
        self: &mut VeilidTransport<T>,
        _addr: Multiaddr,
    ) -> Result<Self::Listener, TransportError<Self::Error>> {
        debug!("VeilidTransport::listen_on");

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
                StreamHandler::run().await;
            });

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

        if let Some(update_receiver) = self.update_receiver.take() {
            let listener = VeilidListener::new(update_receiver, self.api.clone());
            Ok(listener)
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

        let veilid_state_config = get_veilid_state_config(Some(api_clone)).unwrap();
        let node_id = get_my_node_id_from_veilid_state_config(veilid_state_config).unwrap();

        let local_target = cryptotyped_to_target(&node_id);

        match multiaddr_to_target(&addr) {
            Ok(remote_target) => {
                return Ok(async move {
                    let connection = VeilidConnection::new(api, local_target, remote_target)?;

                    connection.connect().await;
                    return Ok(connection);
                }
                .boxed());
            }
            Err(_) => return Err(TransportError::MultiaddrNotSupported(addr)),
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
}
