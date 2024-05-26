use chrono::{DateTime, Duration, Utc};
use futures::executor::block_on;
use futures::future::Ready;

use libp2p::core::transport::ListenerId;
use libp2p::core::transport::TransportEvent;
use libp2p::identity::{Keypair, PublicKey};
use libp2p::Multiaddr;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use veilid_core::Sequencing;
use veilid_core::Target;

use std::task::Poll;
use tokio_crate::sync::{
    mpsc,
    mpsc::{Receiver, Sender},
};
use tokio_crate::task;

use veilid_core::{VeilidAPI, VeilidUpdate};

use crate::address::Address;
use crate::address::DHTStatus;
use crate::connection::VeilidConnection;
use crate::stream::{StreamStatus, VeilidStream};

use crate::utils::get_my_node_id_from_veilid_state_config;
use crate::VeilidTransportType;
use crate::{errors::VeilidError, node_status::NodeStatus};

#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};

pub type VeilidTransportEvent<S> = TransportEvent<Ready<Result<S, VeilidError>>, VeilidError>;

#[derive(Debug, PartialEq)]
enum ListenerStatus {
    Offline,
    Online,
}
#[derive(Debug, PartialEq, Clone)]
pub enum TargetStatus {
    None,
    Republishing(DateTime<Utc>),
    Active(DateTime<Utc>),
}

type Streams = Arc<Mutex<HashMap<Address, Arc<VeilidStream>>>>;

type Event = TransportEvent<Ready<Result<VeilidConnection, VeilidError>>, VeilidError>;

#[derive(Debug)]
pub struct VeilidListener {
    pub transport_type: VeilidTransportType,
    pub id: ListenerId,
    pub local_address: Arc<Mutex<Option<Address>>>,
    pub local_target: Arc<Mutex<(Option<Target>, TargetStatus)>>,
    rx: Receiver<VeilidUpdate>,
    async_tx: Sender<Poll<Option<VeilidTransportEvent<VeilidConnection>>>>,
    async_rx: Receiver<Poll<Option<VeilidTransportEvent<VeilidConnection>>>>,
    api: Arc<VeilidAPI>,
    node_status: Arc<RwLock<NodeStatus>>,
    status: Arc<RwLock<ListenerStatus>>,
    pub streams: Streams,
    my_keypair: Arc<Keypair>,
}

impl VeilidListener {
    pub fn new(
        transport_type: VeilidTransportType,
        id: ListenerId,
        rx: Receiver<VeilidUpdate>,
        api: Arc<VeilidAPI>,
        node_status: Arc<RwLock<NodeStatus>>,
        my_keypair: Arc<Keypair>,
    ) -> Self {
        let (async_tx, async_rx) = mpsc::channel(32);

        Self {
            transport_type,
            id,
            local_address: Arc::new(Mutex::new(None)),
            local_target: Arc::new(Mutex::new((None, TargetStatus::None))),
            rx,
            async_tx,
            async_rx,
            api,
            node_status,
            status: Arc::new(RwLock::new(ListenerStatus::Offline)),
            streams: Arc::new(Mutex::new(HashMap::new())),
            my_keypair,
        }
    }

    fn create_stream(
        api: &Arc<VeilidAPI>,
        listener_id: ListenerId,
        local_address: Address,
        remote_address: Address,
        remote_stream_id: u64,
        streams: &Streams,
        my_keypair: Arc<Keypair>,
    ) -> Result<Arc<VeilidStream>, String> {
        debug!("VeilidListener | create_stream");

        if let Some(remote_target) = block_on(async { remote_address.clone().to_target(api).await })
        {
            info!(
                "VeilidListener | create_stream {:?} | remote_address {:?} | remote_target {:?}",
                remote_stream_id,
                remote_address.to_key(),
                remote_target
            );

            let stream = Arc::new(VeilidStream::new(
                api.clone(),
                listener_id,
                local_address.clone(),
                remote_address.clone(),
                remote_target.clone(),
                remote_stream_id,
                my_keypair,
            ));

            if let Ok(mut guard) = streams.lock() {
                guard.insert(remote_address, stream.clone());
            } else {
                error!("VeilidListener | create_stream | failed to lock streams");
            }
            return Ok(stream);
        } else {
            debug!("VeilidListener | create_stream | failed to fetch remote target");
            return Err("Failed to fetch remote_target".to_string());
        }
    }

    pub fn get_next_event(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Option<Event>> {
        debug!("VeilidListener::get_next_event");
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
                let my_keypair = self.my_keypair.clone();
                let transport_type = self.transport_type.clone();

                let local_address = self.local_address.clone();
                let local_target = self.local_target.clone();

                task::spawn(async move {
                    let result = convert_update(
                        transport_type,
                        local_address,
                        local_target,
                        streams,
                        api,
                        listener_id,
                        update,
                        node_status,
                        listener_status,
                        my_keypair,
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

    pub fn get_local_address(&mut self) -> Option<Address> {
        debug!("VeilidListener::get_local_address");

        let api = &self.api;
        let local_address = match self.local_address.lock() {
            Ok(mut guard) => match *guard {
                Some(ref address) => Some(address.clone()),
                None => {
                    match self.transport_type {
                        VeilidTransportType::Safe => {
                            debug!("VeilidListener::get_local_address | start block_on 1");

                            let safe_addr = block_on(async move {
                                match Address::new_safe(&api).await {
                                    Ok(new_address) => {
                                        *guard = Some(new_address.clone());

                                        return Some(new_address);
                                    }
                                    Err(_) => return None,
                                }
                            });
                            debug!("VeilidListener::get_local_address | finish block_on 1");

                            debug!(
                                "VeilidListener | get_local_address | safe_addr {:?}",
                                safe_addr
                            );

                            return safe_addr;
                        }
                        VeilidTransportType::Unsafe => {
                            let unsafe_addr = if let Ok(guard) = self.node_status.read() {
                                if let Some(my_node_id) = guard.my_node_id() {
                                    Some(Address::new_unsafe(my_node_id))
                                } else {
                                    error!("VeilidListener | get_local_address | failed to get node_id");
                                    None
                                }
                            } else {
                                error!("VeilidListener | get_local_address | failed to read node_status ");
                                None
                            };
                            debug!(
                                "VeilidListener | get_local_address | unsafe_addr {:?}",
                                unsafe_addr
                            );

                            return unsafe_addr;
                        }
                    };
                }
            },
            Err(_) => None,
        };

        let mut local_target = None;
        if let Ok(guard) = self.local_target.lock() {
            local_target = guard.0;
        }
        if local_target.is_none() && local_address.is_some() {
            let api = &self.api;
            let address = local_address.clone();
            info!("VeilidListener | get_local_address | start block_on 2");
            let target = block_on(async move { address.clone().unwrap().to_target(api).await });
            info!("VeilidListener | get_local_address | finish block_on 2");

            if let Ok(mut guard) = self.local_target.lock() {
                guard.0 = target;
                guard.1 = TargetStatus::Active(Utc::now());
            }
        }

        debug!("VeilidListener | get_local_address {:?}", local_address);
        local_address
    }

    pub fn get_node_status(&self) -> Option<NodeStatus> {
        debug!("VeilidListener::get_local_address");

        if let Ok(guard) = self.node_status.read() {
            Some(guard.clone())
        } else {
            None
        }
    }

    pub fn send_ping(&self) {
        debug!("VeilidListener::send_ping");

        let api = self.api.clone();
        let local_address_mutex = self.local_address.clone();
        let local_target_mutex = self.local_target.clone();

        let mut address = None;
        let mut target = None;

        if let Ok(guard) = local_address_mutex.lock() {
            if let Some(local_address) = guard.clone() {
                address = Some(local_address);
            }
        }

        if let Ok(guard) = local_target_mutex.lock() {
            target = guard.0;
        }

        if let (Some(local_address), Some(local_target)) = (address, target) {
            let mut data = b"PING".to_vec();
            data.extend_from_slice(&self.my_keypair.public().encode_protobuf());

            let message_data = VeilidStream::encode_message(
                0,
                0,
                0,
                local_address.clone(),
                data.into(),
                self.my_keypair.clone(),
            );

            task::spawn(async move {
                VeilidListener::send_message_to_me(local_target, api, message_data).await;
                debug!("VeilidStream | send_ping | sent");
            });
        }
    }

    pub async fn send_message_to_me(
        local_target: Target,
        api: Arc<VeilidAPI>,
        message_data: Vec<u8>,
    ) {
        debug!("VeilidListener::send_message_to_me");

        trace!(
            "VeilidStream | send_message_to_me | {:?}",
            String::from_utf8_lossy(&message_data)
        );

        if let Ok(routing_context) = api.routing_context() {
            debug!(
                "VeilidStream | send_message_to_me | Target::PrivateRoute | sending to {:?}",
                local_target
            );

            if let Ok(_) = routing_context
                .with_sequencing(Sequencing::NoPreference)
                .app_message(local_target, message_data)
                .await
            {
                debug!("VeilidStream | send_message_to_me | sent");
            } else {
                error!(
                    "VeilidStream | send_message_to_me | failed to send message to {:?}",
                    local_target
                );
            }
        } else {
            error!("VeilidStream | send_message_to_me | failed to get routing context");
        }
    }

    pub fn check_address_dht_record(&self) {
        debug!("Listener::check_address_dht_record");

        let api = self.api.clone();
        let mut failed_to_fetch_dht = false;

        let my_address = {
            if let Ok(guard) = self.local_address.lock() {
                guard.clone()
            } else {
                None
            }
        };

        debug!(
            "Listener::check_address_dht_record | address {:?}",
            my_address
        );

        if let Some(local_address) = my_address {
            match local_address {
                Address::Unsafe(_) => {}
                Address::Safe(key, _, dht_status) => {
                    let local_address_mutex = self.local_address.clone();
                    let mut should_try_fetch = false;

                    match dht_status {
                        DHTStatus::None => should_try_fetch = true,
                        DHTStatus::Active(last_timestamp) => {
                            if Utc::now().signed_duration_since(last_timestamp)
                                > Duration::seconds(10)
                            {
                                should_try_fetch = true;
                            }
                        }
                    }

                    if should_try_fetch {
                        debug!("Listener::check_address_dht_record | should try fetch = true");

                        let option_my_current_private_route = {
                            if let Ok(guard) = self.local_target.lock() {
                                guard.0
                            } else {
                                None
                            }
                        };

                        task::spawn(async move {
                            if let Ok(routing_context) = api.routing_context() {
                                trace!("Listener::check_address_dht_record  got routing_context");

                                if let Ok(dht_record) =
                                    routing_context.open_dht_record(key, None).await
                                {
                                    trace!("Listener::check_address_dht_record  | open_dht_record {:?}", dht_record);

                                    if let Ok(option_value_data) =
                                        routing_context.get_dht_value(key, 0, false).await
                                    {
                                        trace!(
                                            "Listener::check_address_dht_record  | option_value_data {:?}",
                                            option_value_data
                                        );

                                        if let Some(blob) = option_value_data {
                                            if let Ok(crypto_key) = api
                                                .import_remote_private_route(blob.data().to_vec())
                                            {
                                                info!(
                                                    "Listener::check_address_dht_record  | found target {:?}", crypto_key
                                                );

                                                if let Some(my_current_private_route) =
                                                    option_my_current_private_route
                                                {
                                                    match my_current_private_route {
                                                        Target::NodeId(_) => todo!(),
                                                        Target::PrivateRoute(key) => {
                                                            if key != crypto_key {
                                                                warn!(
                                                                    "Listener::check_address_dht_record  | my target {:?} doesn't match the DHT target {:?}", key, crypto_key
                                                                );
                                                            } else {
                                                                debug!("Listener::check_address_dht_record  | my target matches the DHT target");
                                                                if let Ok(mut guard) =
                                                                    local_address_mutex.lock()
                                                                {
                                                                    if let Some(address) =
                                                                        guard.clone()
                                                                    {
                                                                        match address {
                                                                            Address::Unsafe(_) => {}
                                                                            Address::Safe(
                                                                                key,
                                                                                keypair,
                                                                                ..,
                                                                            ) => {
                                                                                let new_address = Address::Safe(
                                                                                    key,
                                                                                    keypair,
                                                                                    DHTStatus::Active(Utc::now()),
                                                                                );
                                                                                *guard = Some(
                                                                                    new_address,
                                                                                );
                                                                            }
                                                                        }
                                                                        debug!(
                                                                            "Listener::check_address_dht_record  | success"
                                                                        );
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    } else {
                                        warn!("Listener::check_address_dht_record  | failed to get_dht_value");
                                        failed_to_fetch_dht = true;
                                    }
                                } else {
                                    warn!("Listener::check_address_dht_record  | failed to open dht record");
                                    failed_to_fetch_dht = true;
                                }
                            } else {
                                error!("Listener::check_address_dht_record  | failed to getrouting_context");
                            }

                            if failed_to_fetch_dht {
                                warn!("Listener::check_address_dht_record  | failed_to_fetch_dht");

                                if let Ok(mut guard) = local_address_mutex.lock() {
                                    if let Some(address) = guard.clone() {
                                        match address {
                                            Address::Unsafe(_) => {}
                                            Address::Safe(key, keypair, ..) => {
                                                let new_address =
                                                    Address::Safe(key, keypair, DHTStatus::None);
                                                *guard = Some(new_address);
                                            }
                                        }
                                    }
                                }
                                info!(
                                    "Listener::check_address_dht_record  | new address {:?}",
                                    local_address_mutex
                                );
                            }
                            debug!("Listener::check_address_dht_record | end spawn");
                        });
                    }
                }
            }
        }
    }

    pub fn should_update_target(&self) -> bool {
        debug!("VeilidListener::should_update_target");
        let mut should_update = false;

        match self.transport_type {
            VeilidTransportType::Unsafe => {}
            VeilidTransportType::Safe => {
                let local_address_mutex = &self.local_address;
                let local_target_mutex = &self.local_target;

                let local_address = {
                    let mut addr = None;

                    if let Ok(address) = local_address_mutex.lock() {
                        if let Some(local_address) = address.clone() {
                            match local_address {
                                Address::Unsafe(_) => {}
                                Address::Safe(_, _, ref dht_status) => {
                                    addr = Some(local_address.clone());
                                    match dht_status {
                                        DHTStatus::None => should_update = true,
                                        DHTStatus::Active(last_timestamp) => {
                                            if let Ok(guard) = local_target_mutex.lock() {
                                                match guard.1 {
                                                    TargetStatus::None => {}
                                                    TargetStatus::Republishing(_) => {}
                                                    TargetStatus::Active(_) => {
                                                        if Utc::now()
                                                            .signed_duration_since(last_timestamp)
                                                            > Duration::seconds(30)
                                                        {
                                                            warn!(
                                                            "VeilidListener::should_update_target | dht_status {:?} secs ago, should update",
                                                            Utc::now().signed_duration_since(last_timestamp)
                                                        );
                                                            should_update = true;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    addr
                };

                if let Ok(guard) = local_target_mutex.lock() {
                    match guard.1 {
                        TargetStatus::Republishing(last_republish) => {
                            if Utc::now().signed_duration_since(last_republish)
                                > Duration::seconds(10)
                            {
                                warn!(
                                    "VeilidListener::should_update_target | Last republish {:?} secs ago, should update",
                                    Utc::now().signed_duration_since(last_republish)
                                );
                                should_update = true;
                            };
                        }
                        TargetStatus::Active(last_timestamp) => {
                            if Utc::now().signed_duration_since(last_timestamp)
                                > Duration::seconds(30)
                            {
                                warn!(
                                    "VeilidListener::should_update_target | my private route last active {:?} secs ago, should update",
                                    Utc::now().signed_duration_since(last_timestamp)
                                );
                                should_update = true;
                            };
                        }
                        TargetStatus::None => {
                            if local_address.is_some() {
                                warn!(
                                    "VeilidListener::should_update_target | TargetStatus::None, should update",
                                );
                                should_update = true;
                            }
                        }
                    }

                    if guard.0.is_none() {
                        if local_address.is_some() {
                            warn!(
                                "VeilidListener::should_update_target | no target, should update"
                            );
                            should_update = true;
                        }
                    }
                } else {
                    error!("VeilidListener::should_update_target | failed to lock local_target");
                }
            }
        }

        should_update
    }

    pub fn update_target(&mut self, api: Arc<VeilidAPI>) {
        debug!("VeilidListener::update_target");
        let local_target_mutex = self.local_target.clone();
        let local_address_mutex = self.local_address.clone();

        info!(
            "VeilidListener::update_target | current_target {:?}",
            local_target_mutex
        );

        {
            if let Ok(mut guard) = local_target_mutex.lock() {
                guard.1 = TargetStatus::Republishing(Utc::now());
            }
        }

        if let Some(current_address) = self.get_local_address() {
            task::spawn(async move {
                if let Ok(new_target) = Address::update_safe(&api, current_address.clone()).await {
                    debug!("VeilidListener::update_target | target successfully updated");

                    if let Ok(mut guard) = local_target_mutex.lock() {
                        guard.0 = Some(new_target);
                        guard.1 = TargetStatus::Active(Utc::now());
                    } else {
                        error!("VeilidListener::update_target | failed to lock local_target_mutex");
                    }

                    if let Ok(mut guard) = local_address_mutex.lock() {
                        if let Some(local_address) = guard.clone() {
                            match local_address {
                                Address::Unsafe(_) => {}
                                Address::Safe(key, keypair, ..) => {
                                    let new_address =
                                        Address::Safe(key, keypair, DHTStatus::Active(Utc::now()));
                                    *guard = Some(new_address);
                                }
                            }
                        }
                    }

                    info!(
                        "VeilidListener::update_target | new_address {:?}",
                        local_address_mutex
                    );

                    info!(
                        "VeilidListener::update_target | new_target {:?}",
                        local_target_mutex
                    );
                } else {
                    warn!("VeilidListener::update_target | target update failed, deleting local_target");
                    if let Ok(mut guard) = local_target_mutex.lock() {
                        guard.0 = None;
                        guard.1 = TargetStatus::None;
                    }
                }
            });
        } else {
            error!("VeilidListener::update_target | failed to get local_address");
        }
    }
}

async fn convert_update(
    transport_type: VeilidTransportType,
    local_address_mutex: Arc<Mutex<Option<Address>>>,
    local_target_mutex: Arc<Mutex<(Option<Target>, TargetStatus)>>,
    streams: Streams,
    api: Arc<VeilidAPI>,
    listener_id: ListenerId,
    update: VeilidUpdate,
    node_status: Arc<RwLock<NodeStatus>>,
    listener_status: Arc<RwLock<ListenerStatus>>,
    my_keypair: Arc<Keypair>,
) -> Poll<Option<VeilidTransportEvent<VeilidConnection>>> {
    match update {
        VeilidUpdate::Log(_) => {
            debug!("VeilidUpdate | Log");
            Poll::Pending
        }

        VeilidUpdate::AppMessage(app_msg) => {
            debug!("VeilidUpdate | AppMessage");
            trace!("VeilidUpdate | AppMessage : {:?}", app_msg);

            // decode the payload
            let (delivered_seq, seq, stream_id, remote_address, data, signature) =
                match VeilidStream::decode_message(&app_msg.message()) {
                    Ok((r, s, i, a, d, sig)) => (r, s, i, a, d, sig),
                    Err(e) => {
                        error!("VeilidUpdate | AppMessage {:?}", e);
                        return Poll::Pending;
                    }
                };

            trace!("VeilidUpdate | AppMessage | streams {:?}", streams);

            // fetch the stream, if it exists
            let option_stream = {
                let mutex_guard = streams.lock().unwrap();
                mutex_guard.get(&remote_address).map(Arc::clone)
            };

            // extract outbound_last_seq and remote_public_key from the stream
            let (outbound_last_seq, remote_public_key) = match option_stream {
                Some(ref stream) => (
                    stream.get_outbound_last_seq(),
                    stream.remote_public_key.clone(),
                ),
                None => (0, Arc::new(Mutex::new(None))),
            };

            // use the remote_public_key to check if the data payload is signed
            let is_signed = VeilidStream::verify_signature(remote_public_key, &data, &signature);

            if stream_id == 0 {
                info!(
                    "VeilidUpdate | AppMessage | received self ping | from my_address {:?}",
                    remote_address.to_key(),
                );
            } else {
                info!(
                    "VeilidUpdate | AppMessage | they have {:?} of {:?} | they sent {:?} | bytes {:?} on stream_id {:?} | is_signed {:?} | from remote_address {:?}",
                    delivered_seq,
                    outbound_last_seq,
                    seq,
                    data.len(),
                    stream_id,
                    is_signed,
                    remote_address.to_key(),
                );
            }

            trace!(
                "VeilidUpdate | AppMessage | data {:?}",
                String::from_utf8_lossy(&data)
            );

            enum MessageType {
                Dial,
                Listen,
                Status,
                Ping,
                Data,
            }

            // Determine message type
            let message_type = if String::from_utf8_lossy(&data).starts_with("DIAL") {
                MessageType::Dial
            } else if String::from_utf8_lossy(&data).starts_with("LISTEN") {
                MessageType::Listen
            } else if String::from_utf8_lossy(&data).starts_with("STATUS") {
                MessageType::Status
            } else if String::from_utf8_lossy(&data).starts_with("PING") {
                MessageType::Ping
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

            // fetch this node's local_address
            let local_address = if let Ok(guard) = local_address_mutex.lock() {
                if guard.is_none() {
                    error!("VeilidUpdate::AppMessage | Listener missing local address");
                    return Poll::Ready(None);
                } else {
                    guard
                }
            } else {
                return Poll::Ready(None);
            }
            .clone()
            .unwrap();

            match option_stream {
                None => {
                    match message_type {
                        MessageType::Dial => {
                            // Happy Path
                            debug!("VeilidUpdate | AppMessage | Stream None | received DIAL");

                            let remote_stream_id = stream_id;

                            if let Ok(stream) = VeilidListener::create_stream(
                                &api,
                                listener_id,
                                local_address,
                                remote_address,
                                remote_stream_id,
                                &streams,
                                my_keypair,
                            ) {
                                if &data[..4] == b"DIAL" {
                                    // Proceed to extract the key
                                    let key_bytes = &data[4..]; // Slice after the first 4 bytes

                                    // Decode the protobuf-encoded public key
                                    if let Ok(public_key) =
                                        PublicKey::try_decode_protobuf(key_bytes)
                                    {
                                        stream.update_remote_public_key(public_key);
                                    }
                                }

                                stream
                                    .update_remote_stream_id(stream_id)
                                    .update_status(StreamStatus::Listen)
                                    .send_listen()
                                    .await;

                                return stream.activate();
                            }
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
                        MessageType::Ping => {
                            debug!("VeilidUpdate | AppMessage | Stream None | received PING | updating timestamp");

                            if let Ok(mut guard) = local_target_mutex.lock() {
                                *guard = (guard.0, TargetStatus::Active(Utc::now()));
                            }
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

                                        if &data[..4] == b"DIAL" {
                                            // Proceed to extract the key
                                            let key_bytes = &data[4..]; // Slice after the first 4 bytes

                                            // Decode the protobuf-encoded public key
                                            if let Ok(public_key) =
                                                PublicKey::try_decode_protobuf(key_bytes)
                                            {
                                                stream.update_remote_public_key(public_key);
                                            }
                                        }

                                        // Need to choose role based on ID
                                        match remote_address.cmp(&local_address) {
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
                                                error!(
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

                                                return stream.activate();
                                            }
                                        };
                                    }
                                    MessageType::Listen => {
                                        // Happy Path
                                        debug!("VeilidUpdate | AppMessage | Stream DIAL | received LISTEN");

                                        if &data[..6] == b"LISTEN" {
                                            // Proceed to extract the key
                                            let key_bytes = &data[6..]; // Slice after the first 6 bytes

                                            // Decode the protobuf-encoded public key
                                            if let Ok(public_key) =
                                                PublicKey::try_decode_protobuf(key_bytes)
                                            {
                                                stream.update_remote_public_key(public_key);
                                            }
                                        }

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
                                    MessageType::Ping => {
                                        warn!("VeilidUpdate | AppMessage | Stream DIAL | received PING | ignoring");
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

                                            if is_signed {
                                                stream
                                                    .update_status(StreamStatus::Active)
                                                    .recv_message(delivered_seq, seq, data);
                                            } else {
                                                warn!("VeilidUpdate | AppMessage | Stream LISTEN | received DATA | not signed, discarding");
                                            }
                                        } else {
                                            debug!("VeilidUpdate | AppMessage | Stream LISTEN | received DATA | NOT matching stream_id | ignoring");
                                        }
                                    }
                                    MessageType::Ping => {
                                        warn!("VeilidUpdate | AppMessage | Stream LISTEN | received PING | ignoring");
                                    }
                                }
                            }

                            StreamStatus::Active => {
                                match message_type {
                                    MessageType::Dial => {
                                        if stream.get_remote_stream_id() < stream_id {
                                            debug!(
                                            "VeilidUpdate | AppMessage | Stream ACTIVE | received DIAL | New stream"
                                        );

                                            let remote_stream_id = stream_id;

                                            if let Ok(stream) = VeilidListener::create_stream(
                                                &api,
                                                listener_id,
                                                local_address,
                                                remote_address,
                                                remote_stream_id,
                                                &streams,
                                                my_keypair,
                                            ) {
                                                if &data[..4] == b"DIAL" {
                                                    // Proceed to extract the key
                                                    let key_bytes = &data[4..]; // Slice after the first 4 bytes

                                                    // Decode the protobuf-encoded public key
                                                    if let Ok(public_key) =
                                                        PublicKey::try_decode_protobuf(key_bytes)
                                                    {
                                                        stream.update_remote_public_key(public_key);
                                                    }
                                                }

                                                stream
                                                    .update_remote_stream_id(stream_id)
                                                    .update_status(StreamStatus::Listen)
                                                    .send_listen()
                                                    .await;

                                                return stream.activate();
                                            }
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
                                        if is_signed {
                                            stream
                                                .update_status(StreamStatus::Active)
                                                .recv_message(delivered_seq, seq, data);
                                        } else {
                                            warn!("VeilidUpdate | AppMessage | Stream ACTIVE | received DATA | not signed, discarding");
                                        }
                                    }
                                    MessageType::Ping => {
                                        warn!("VeilidUpdate | AppMessage | Stream ACTIVE | received PING | ignoring");
                                    }
                                }
                            }
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
                                MessageType::Ping => {
                                    warn!("VeilidUpdate | AppMessage | Stream EXPIRED | received PING | ignoring");
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
            let mut is_online = false;
            let mut listen_addr = Multiaddr::empty();
            let mut try_online = false;

            {
                let mut node_status_lock = node_status.write().unwrap();

                let is_attached = match attach_state.state {
                    veilid_core::AttachmentState::Detached => false,
                    veilid_core::AttachmentState::Attaching => false,
                    veilid_core::AttachmentState::AttachedWeak => true,
                    veilid_core::AttachmentState::AttachedGood => true,
                    veilid_core::AttachmentState::AttachedStrong => true,
                    veilid_core::AttachmentState::FullyAttached => true,
                    veilid_core::AttachmentState::OverAttached => true,
                    veilid_core::AttachmentState::Detaching => false,
                };

                if node_status_lock.is_attached() != &is_attached {
                    node_status_lock.update_is_attached(&is_attached);
                }

                let public_internet_ready = attach_state.public_internet_ready;

                if node_status_lock.public_internet_ready() != &public_internet_ready {
                    node_status_lock.update_public_internet_ready(&public_internet_ready);
                }

                let listener_status_guard = listener_status.read().unwrap();
                if *listener_status_guard == ListenerStatus::Offline
                    && public_internet_ready
                    && is_attached
                {
                    try_online = true;
                }
            }

            if try_online {
                info!("NodeStatus | try online");

                match transport_type {
                    VeilidTransportType::Unsafe => {
                        let node_status_lock = node_status.read().unwrap();

                        if let Some(my_node_id) = node_status_lock.my_node_id() {
                            listen_addr = Address::new_unsafe(my_node_id).to_multiaddr();

                            is_online = true;
                        }
                    }

                    VeilidTransportType::Safe => {
                        trace!("VeilidUpdate::Attachment | API");
                        if let Ok(local_address) = Address::new_safe(&api).await {
                            if let Ok(mut guard) = local_address_mutex.lock() {
                                *guard = Some(local_address.clone());
                            } else {
                                error!(
                                    "VeilidUpdate::Attachment | failed to lock local_address_mutex"
                                );
                            }

                            let current_option_target = if let Ok(guard) = local_target_mutex.lock()
                            {
                                guard.0
                            } else {
                                None
                            };

                            if current_option_target.is_none() {
                                let new_local_target = local_address.to_target(&api).await;
                                if new_local_target.is_some() {
                                    if let Ok(mut guard) = local_target_mutex.lock() {
                                        guard.0 = new_local_target;
                                        guard.1 = TargetStatus::Active(Utc::now());
                                    }
                                }
                            }

                            listen_addr = local_address.to_multiaddr();

                            info!("VeilidUpdate::Attachment | listen_addr {:?}", listen_addr);

                            is_online = true;
                        }
                    }
                };
            }

            match is_online {
                false => {
                    trace!("NodeStatus | offline");
                    warn!("VeilidUpdate::Attachment | Need to add !is_online and remove the Swarm listener");
                    return Poll::Pending;
                }
                true => {
                    info!("NodeStatus | online");

                    let mut listener_status_guard = listener_status.write().unwrap();

                    *listener_status_guard = ListenerStatus::Online;

                    return Poll::Ready(Some(VeilidTransportEvent::NewAddress {
                        listener_id,
                        listen_addr,
                    }));
                }
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

            if node_status.my_node_id() != &Some(my_node_id.clone()) {
                node_status.update_my_node_id(&my_node_id);

                match transport_type {
                    VeilidTransportType::Safe => {}
                    VeilidTransportType::Unsafe => {
                        let new_address = Address::new_unsafe(&my_node_id);

                        if let Ok(mut guard) = local_address_mutex.lock() {
                            *guard = Some(new_address);
                            info!(
                                "VeilidUpdate::Config | local_address {:?}",
                                local_address_mutex
                            );
                        } else {
                            error!("VeilidUpdate::Config | failed to lock local_address");
                        }
                    }
                }

                Poll::Pending
            } else {
                return Poll::Pending;
            }
        }

        VeilidUpdate::RouteChange(veilid_route_change) => {
            debug!("VeilidUpdate: RouteChange: {:?}", veilid_route_change);

            let mut current_local_address = None;

            {
                if let Ok(guard) = local_address_mutex.lock() {
                    current_local_address = guard.as_ref().cloned()
                } else {
                    debug!("VeilidUpdate | AppMessage | local_address could not be locked");
                };
            }

            trace!(
                "VeilidUpdate | RouteChange | current_local_address {:?}",
                current_local_address
            );

            if let Some(current_address) = current_local_address {
                if let Some(my_target) = current_address.clone().to_target(&api).await {
                    match my_target {
                        veilid_core::Target::NodeId(_) => {}
                        veilid_core::Target::PrivateRoute(crypto_key) => {
                            let should_republish_dead_remote_routes =
                                veilid_route_change.dead_remote_routes.contains(&crypto_key);

                            if should_republish_dead_remote_routes {
                                debug!(
                                    "VeilidUpdate: RouteChange: should_republish_dead_remote_routes {:?} | found target {:?}",
                                        should_republish_dead_remote_routes, my_target
                                    );
                            }

                            let should_republish_dead_routes =
                                veilid_route_change.dead_routes.contains(&crypto_key);

                            if should_republish_dead_routes {
                                warn!(
                                    "VeilidUpdate: RouteChange: should_republish_dead_routes {:?} | found target {:?}",
                                    should_republish_dead_routes, my_target
                                );
                            }

                            // if should_republish_dead_routes || should_republish_dead_remote_routes {
                            //     debug!("VeilidUpdate: RouteChange: {:?}", veilid_route_change);
                            //     debug!("VeilidUpdate | RouteChange | my_target {:?}", my_target);
                            //     debug!(
                            //         "VeilidUpdate: RouteChange | dead target, but NOT republishing"
                            //     );
                            // }
                        }
                    }
                }
            } else {
                error!("VeilidUpdate::RouteChange | listener missing local_address");
            }

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
