// use prost::Message;

use futures::future;
use libp2p::identity::{Keypair, PublicKey};
use prost::Message;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use libp2p::core::transport::{ListenerId, TransportEvent};
use std::task::{Context, Poll, Waker};
use tokio_crate::task;
use tokio_crate::time::{Duration, Instant};
use veilid_core::{CryptoKey, CryptoTyped, Target, VeilidAPI};

#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};

use crate::connection::VeilidConnection;
use crate::listener::VeilidTransportEvent;
use crate::proto::Payload;
use crate::stream_seq::StreamSeq;
use crate::utils::{
    cryptotyped_to_multiaddr, cryptotyped_to_target, get_my_node_id_from_veilid_state_config,
    get_veilid_state_config,
};
use crate::SETTINGS;

// This mod creates an inbound and outbound stream of data between
// the local and remote nodes. Processes fetch a stream by its remote target
// using VeilidStreamManager

#[derive(Debug, PartialEq, Clone)]
pub enum StreamStatus {
    Dial,
    Listen,
    Active,
    Expired,
}

#[derive(Debug, Clone)]
pub struct OutboundMessage {
    pub seq: u32,
    pub data: Vec<u8>,
    pub last_sent: Option<Instant>,
}

#[derive(Debug)]
pub struct VeilidStream {
    // reference to Veilid's API
    pub api: Arc<VeilidAPI>,
    // this node's keys
    pub my_keypair: Arc<Keypair>,
    // remote's public key for signature validation
    pub remote_public_key: Arc<Mutex<Option<PublicKey>>>,
    // the address of the remote Veilid node
    pub remote_target: Target,
    // my id for this stream
    pub my_stream_id: u64,
    // remote's id for this stream
    pub remote_stream_id: Arc<Mutex<u64>>,
    // handle to wake AsyncRead for VeilidConnection
    pub waker: Arc<Mutex<Option<Waker>>>,
    //
    // INBOUND
    // inbound slices that are complete and readable
    inbound_stream: Arc<Mutex<Vec<u8>>>,
    // inbound slices that are incomplete and are awaiting furter data delivery
    inbound_buffer: Arc<Mutex<Vec<u8>>>,
    // seq cursor of contiguous data
    pub inbound_received_seq: Arc<StreamSeq>,
    // messages not ready to be processed
    inbound_message_queue: Arc<Mutex<HashMap<u32, Vec<u8>>>>,
    // last inbound message received timestamp
    inbound_last_timestamp: Arc<Mutex<Instant>>,
    //
    // OUTBOUND
    // oubound slices that are complete
    outbound_stream: Arc<Mutex<Vec<u8>>>,
    // messages not yet successfully delivered
    outbound_message_queue: Arc<Mutex<HashMap<u32, OutboundMessage>>>,
    // last seq sent to remote
    pub outbound_last_seq: Arc<StreamSeq>,
    // last known seq successfully delivered
    pub outbound_delivered_seq: Arc<StreamSeq>,
    // last outbound message sent timestamp
    outbound_last_timestamp: Arc<Mutex<Instant>>,
    // so the connection can close
    pub status: Arc<Mutex<StreamStatus>>,
}

impl VeilidStream {
    pub fn new(
        api: Arc<VeilidAPI>,
        remote_target: Target,
        remote_stream_id: u64,
        keypair: Arc<Keypair>,
    ) -> Self {
        let my_stream_id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        debug!("VeilidStream | new | my_stream_id {:?}", my_stream_id);

        Self {
            api,
            remote_target,
            my_stream_id,
            remote_stream_id: Arc::new(Mutex::new(remote_stream_id)),
            my_keypair: keypair,
            remote_public_key: Arc::new(Mutex::new(None)),

            inbound_stream: Arc::new(Mutex::new(Vec::new())),
            inbound_buffer: Arc::new(Mutex::new(Vec::new())),
            inbound_received_seq: Arc::new(StreamSeq::new(0)),
            inbound_message_queue: Arc::new(Mutex::new(HashMap::new())),
            inbound_last_timestamp: Arc::new(Mutex::new(Instant::now())),

            outbound_stream: Arc::new(Mutex::new(Vec::new())),
            outbound_message_queue: Arc::new(Mutex::new(HashMap::new())),
            outbound_last_seq: Arc::new(StreamSeq::new(0)),
            outbound_delivered_seq: Arc::new(StreamSeq::new(0)),
            outbound_last_timestamp: Arc::new(Mutex::new(Instant::now())),

            waker: Arc::new(Mutex::new(None)),
            status: Arc::new(Mutex::new(StreamStatus::Dial)),
        }
    }

    pub fn generate_random_u32() -> u32 {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        rng.gen::<u32>()
    }

    pub fn update_remote_stream_id(self: &Arc<VeilidStream>, id: u64) -> Arc<Self> {
        debug!("VeilidStream | update_remote_stream_id {:?}", id);
        let mut remote_stream_id = self.remote_stream_id.lock().unwrap();
        *remote_stream_id = id;
        self.clone()
    }

    pub fn update_inbound_last_timestamp_to_now(self: &Arc<VeilidStream>) -> Arc<Self> {
        debug!("VeilidStream | update_inbound_last_timestamp_to_now");

        let mut last_active = self.inbound_last_timestamp.lock().unwrap();
        *last_active = Instant::now();
        self.clone()
    }

    pub fn update_outbound_last_timestamp_to_now(self: &Arc<VeilidStream>) -> Arc<Self> {
        debug!("VeilidStream | update_outbound_last_timestamp_to_now");

        let mut last_active = self.outbound_last_timestamp.lock().unwrap();
        *last_active = Instant::now();
        self.clone()
    }

    pub fn update_status(self: &Arc<VeilidStream>, status: StreamStatus) -> Arc<Self> {
        debug!("VeilidStream | update_status");

        let mut stream_status = self.status.lock().unwrap();

        *stream_status = status;
        self.clone()
    }

    pub fn update_remote_public_key(self: &Arc<VeilidStream>, key: PublicKey) -> Arc<Self> {
        debug!("VeilidStream | update_remote_public_key");
        let mut remote_public_key = self.remote_public_key.lock().unwrap();

        *remote_public_key = Some(key);
        self.clone()
    }

    pub fn is_pending(&self) -> bool {
        // debug!("VeilidStream | is_pending");

        if let Ok(guard) = self.status.lock() {
            if *guard != StreamStatus::Active && *guard != StreamStatus::Expired {
                debug!("VeilidStream | is_pending | StreamStatus::Pending");
                return true;
            } else {
                return false;
            }
        } else {
            return false;
        }
    }

    pub fn is_active(&self) -> bool {
        // debug!("VeilidStream | is_active");
        // the stream is active if we've received a message within the timeout deadline and it's not expired or pending

        if let Ok(guard) = self.status.lock() {
            if *guard != StreamStatus::Active {
                debug!("VeilidStream | is_active | NOT StreamStatus::Active");
                return false;
            }
        }

        let seconds = SETTINGS.stream_timeout_secs;
        let last_active = self.inbound_last_timestamp.lock().unwrap();
        let is_active = *last_active > Instant::now() - Duration::new(seconds, 0);

        let duration_since_last_active = Instant::now().duration_since(*last_active);
        let secs_ago = duration_since_last_active.as_secs();

        debug!(
            "VeilidStream | is_active {:?} | last message {:?} secs ago",
            is_active, secs_ago
        );
        is_active
    }

    pub fn is_expired(&self) -> bool {
        // debug!("VeilidStream | is_active");
        // the stream is active if we've received a message within the timeout deadline and it's not expired or pending

        if let Ok(guard) = self.status.lock() {
            if *guard == StreamStatus::Expired {
                debug!("VeilidStream | is_expired | StreamStatus::Expired");
                return false;
            }
        }

        let seconds = SETTINGS.stream_timeout_secs;
        let last_active = self.inbound_last_timestamp.lock().unwrap();
        let is_active = *last_active > Instant::now() - Duration::new(seconds, 0);

        let duration_since_last_active = Instant::now().duration_since(*last_active);
        let secs_ago = duration_since_last_active.as_secs();

        debug!(
            "VeilidStream | is_expired {:?} | last message {:?} secs ago",
            !is_active, secs_ago
        );
        !is_active
    }

    pub fn get_status(&self) -> Result<StreamStatus, String> {
        if let Ok(guard) = self.status.lock() {
            match *guard {
                StreamStatus::Dial => return Ok(StreamStatus::Dial),
                StreamStatus::Listen => return Ok(StreamStatus::Listen),
                StreamStatus::Active => {
                    let seconds = SETTINGS.stream_timeout_secs;
                    let last_active = self.inbound_last_timestamp.lock().unwrap();
                    let is_active = *last_active > Instant::now() - Duration::new(seconds, 0);

                    let duration_since_last_active = Instant::now().duration_since(*last_active);
                    let secs_ago = duration_since_last_active.as_secs();

                    debug!(
                        "VeilidStream | get_status {:?} | last message {:?} secs ago",
                        is_active, secs_ago
                    );

                    if is_active {
                        return Ok(StreamStatus::Active);
                    } else {
                        return Ok(StreamStatus::Expired);
                    }
                }
                StreamStatus::Expired => {
                    return Ok(StreamStatus::Expired);
                }
            }
        } else {
            error!("VeilidStream | get_status | could not get lock");
            return Err(String::from("Could not lock status"));
        }
    }

    pub fn get_remote_stream_id(&self) -> u64 {
        let id = self.remote_stream_id.lock().unwrap();
        *id
    }

    pub fn wake_to_read(&self) {
        debug!("VeilidStream | wake_to_read");

        if let Ok(option) = self.waker.lock() {
            if let Some(waker) = &*option {
                waker.clone().wake();
            } else {
                debug!("No waker set to wake");
            }
        } else {
            error!("Failed to acquire lock on waker");
        }
    }

    pub fn activate(
        self: &Arc<VeilidStream>,
        sender: &CryptoTyped<CryptoKey>,
        remote_target: Target,
        listener_id: ListenerId,
    ) -> Poll<Option<VeilidTransportEvent<VeilidConnection>>> {
        let api = &self.api;
        let veilid_state_config = get_veilid_state_config(Some(api.clone())).unwrap();
        let node_id = get_my_node_id_from_veilid_state_config(veilid_state_config).unwrap();
        let local_addr = cryptotyped_to_multiaddr(&node_id);
        let local_target = cryptotyped_to_target(&node_id);

        let remote_addr = cryptotyped_to_multiaddr(sender);

        let connection =
            VeilidConnection::new(local_target.clone(), remote_target.clone(), self.clone())
                .unwrap();

        return Poll::Ready(Some(TransportEvent::Incoming {
            listener_id,
            upgrade: future::ok(connection),
            local_addr,
            send_back_addr: remote_addr,
        }));
    }

    // Inbound
    pub fn decode_message(
        packet: &[u8],
        public_key: Arc<Mutex<Option<PublicKey>>>,
    ) -> Result<(u32, u32, u64, Vec<u8>, bool), String> {
        debug!("VeilidStream | decode_message");

        match Payload::decode(packet) {
            Ok(payload) => {
                trace!("VeilidStream | decode_message | payload {:?}", payload);

                let Payload {
                    received_seq,
                    msg_seq,
                    stream_id,
                    signature,
                    data,
                } = payload;

                trace!(
                    "VeilidStream | decode_message | public key {:?}",
                    public_key
                );

                let is_signed = match public_key.lock().unwrap().clone() {
                    Some(key) => key.verify(&data, &signature),
                    None => false,
                };

                info!(
                    "VeilidStream | decode_message | they have {:?} | {:?} bytes {:?} from id {:?} | is signed {:?}",
                    received_seq,
                    msg_seq,
                    data.len(),
                    stream_id,
                    is_signed
                );
                Ok((received_seq, msg_seq, stream_id, data, is_signed))
            }
            Err(e) => {
                error!("VeilidStream | decode_message {:?}", e);
                return Err(e.to_string());
            }
        }
    }

    pub fn recv_message(self: &Arc<VeilidStream>, delivered_seq: u32, seq: u32, data: Vec<u8>) {
        // if !self.is_active() {
        //     debug!("VeilidStream | recv_message | expired");
        //     return StreamStatus::Expired;
        // }

        debug!(
            "VeilidStream | recv_message | delivered_seq: {:?} seq {:?} bytes {:?}",
            delivered_seq,
            seq,
            data.len()
        );

        let mut should_update_inbound_timestamp = false;

        // check if we need this payload
        let inbound_received_seq = self.get_inbound_received_seq();
        debug!(
            "VeilidStream | recv_message | inbound_received_seq {:?}",
            inbound_received_seq
        );

        let mut final_seq = inbound_received_seq;

        if seq <= inbound_received_seq {
            // discard duplicates
            debug!("VeilidStream | recv_message | duplicate");
        } else if seq > inbound_received_seq + 1 {
            if seq < inbound_received_seq + 20 {
                // store it for later if it's one of the next 20 messages
                debug!("VeilidStream | recv_message | store for later {:?}", seq);

                let result = self.inbound_message_queue.lock();
                match result {
                    Ok(mut queue) => {
                        queue.insert(seq, data);
                    }
                    Err(e) => {
                        error!("VeilidStream | recv_message {:?}", e);
                        // return StreamStatus::Expired;
                    }
                }
            } else {
                debug!("VeilidStream | recv_message | ignore {:?}", seq);
            }
        } else {
            if self.is_active() {
                // receive it
                self.recv_inbound_buffer(&data);
                should_update_inbound_timestamp = true;
                final_seq += 1;

                debug!(
                    "VeilidStream | recv_message | received {:?} bytes {:?}",
                    seq,
                    data.len()
                );

                trace!(
                    "VeilidStream | recv_message | received {:?}",
                    String::from_utf8_lossy(&data)
                );

                // do we have the next payloads in our cache?
                let result = self.inbound_message_queue.lock();

                match result {
                    Ok(mut queue) => {
                        // extract from queue and add to buffer
                        while let Some(data) = queue.remove(&(final_seq + 1)) {
                            self.recv_inbound_buffer(&data);
                            debug!(
                                "VeilidStream | recv_message | received another {:?}",
                                final_seq + 1
                            );

                            final_seq += 1;
                        }
                    }
                    Err(e) => {
                        error!("VeilidStream | recv_message {:?}", e);
                    }
                }

                // update stream
                self.update_inbound_received_seq(final_seq)
                    .update_outbound_delivered_seq(delivered_seq)
                    .remove_sent_messages_from_queue()
                    .update_inbound_stream();
            }
        };
        if should_update_inbound_timestamp {
            self.update_inbound_last_timestamp_to_now();
        }
    }

    pub fn recv_inbound_buffer(&self, data: &[u8]) {
        debug!("VeilidStream | recv_inbound_buffer | start");

        // Obtain a lock on the read_buffer.
        let mut read_buffer = self.inbound_buffer.lock().unwrap();

        // Append incoming data to the buffer.
        read_buffer.extend_from_slice(data);

        drop(read_buffer);

        trace!(
            "VeilidStream | recv_inbound_buffer | data {:?}",
            String::from_utf8_lossy(data)
        );

        debug!(
            "VeilidStream | recv_inbound_buffer | bytes {:?}",
            data.len()
        );
    }

    pub fn update_inbound_stream(self: &Arc<VeilidStream>) -> Arc<Self> {
        trace!("VeilidStream | update_inbound_stream | start");

        // Lock the inbound_buffer to get access to the buffer
        let mut inbound_buffer_guard = self.inbound_buffer.lock().unwrap();

        // Create a temporary vector to hold complete slices
        let mut complete_slices = Vec::new();

        // While there's enough data in the buffer to possibly contain a complete slice
        while !inbound_buffer_guard.is_empty() {
            // Read the slice length from the header
            if inbound_buffer_guard.len() < 4 {
                // Not enough data for a header; exit the function or handle the error
                error!("VeilidStream | update_inbound_stream | Not enough data for a u32 header");
                return self.clone();
            }

            let slice_len_bytes = [
                inbound_buffer_guard[0],
                inbound_buffer_guard[1],
                inbound_buffer_guard[2],
                inbound_buffer_guard[3],
            ];

            let slice_len = u32::from_le_bytes(slice_len_bytes) as usize;

            trace!(
                "VeilidStream | update_inbound_stream | inbound_buffer {:?}",
                String::from_utf8_lossy(&inbound_buffer_guard.clone())
            );

            debug!(
                "VeilidStream | update_inbound_stream | slice_len {:?}",
                slice_len
            );

            // Check if the buffer contains the complete slice
            if inbound_buffer_guard.len() >= slice_len + 4 {
                // Remove the slice (including its header) from the inbound_buffer
                let mut slice: Vec<u8> = inbound_buffer_guard.drain(..(4 + slice_len)).collect();

                // remove our u32
                slice.drain(..4);

                trace!(
                    "VeilidStream | update_inbound_stream | slice {:?}",
                    String::from_utf8_lossy(&slice.clone())
                );

                // Store the complete slice to later insert to the stream
                complete_slices.push(slice.to_vec());
            } else {
                // Not enough data for a complete slice; exit the loop
                break;
            }
        }

        // Unlock the inbound_buffer_guard to release the lock
        drop(inbound_buffer_guard);

        // Log the length of the data moved to the inbound_stream
        debug!(
            "VeilidStream | update_inbound_stream | slices {:?}",
            complete_slices.len()
        );

        if !complete_slices.is_empty() {
            // Lock the inbound_stream to get access to the stream buffer
            let mut inbound_stream_guard = self.inbound_stream.lock().unwrap();
            for slice in complete_slices {
                // Move complete slices to the inbound_stream
                inbound_stream_guard.extend(slice);
            }

            trace!(
                "VeilidStream | update_inbound_stream | stream {:?}",
                String::from_utf8_lossy(&inbound_stream_guard)
            );
            // Unlock the inbound_stream_guard to release the lock
            drop(inbound_stream_guard);

            // Potentially wake up any tasks waiting for data
            self.wake_to_read();
        }

        self.clone()
    }

    pub fn read_inbound_stream(&self, _: &mut Context<'_>, buf: &mut [u8]) -> Option<usize> {
        let mut stream = self.inbound_stream.lock().unwrap();
        trace!(
            "VeilidStream | read_inbound_stream | stream {:?}",
            String::from_utf8_lossy(&stream)
        );
        if stream.is_empty() {
            debug!("VeilidStream | read_inbound_stream | empty");
            // *self.waker.lock().unwrap() = Some(cx.waker().clone());
            None
        } else {
            let readable = std::cmp::min(buf.len(), stream.len());

            debug!(
                "VeilidStream | read_inbound_stream | buf.len() {:?} | guard.len() {:?} | readable {:?}",
                buf.len(), stream.len(), readable
            );

            let data: Vec<u8> = stream.drain(..readable).collect();

            trace!(
                "VeilidStream | read_inbound_stream | {:?}",
                String::from_utf8_lossy(&data)
            );
            buf[..readable].copy_from_slice(&data);
            Some(readable)
        }
    }

    pub fn remove_sent_messages_from_queue(self: &Arc<VeilidStream>) -> Arc<Self> {
        debug!("VeilidStream | remove_sent_messages_from_queue");

        let delivered_seq = self.get_outbound_delivered_seq();

        let mut queue = self.outbound_message_queue.lock().unwrap();

        // Retain only the messages whose sequence number is greater than delivered_seq
        let initial_count = queue.len();
        queue.retain(|&seq, _| seq > delivered_seq);

        // Calculate the number of removed messages for debugging purposes
        let count = initial_count - queue.len();

        if count > 0 {
            debug!(
                "VeilidStream | remove_sent_messages_from_queue | removed {:?}",
                count
            );
        }

        self.clone()
    }

    //
    //
    //
    //
    //
    //
    // Outbound

    pub async fn send_dial(self: &Arc<VeilidStream>) {
        let api = &self.api;
        let remote_target = self.remote_target;
        let my_stream_id = self.my_stream_id;

        let mut data = b"DIAL".to_vec();
        data.extend_from_slice(&self.my_keypair.public().encode_protobuf());

        let message_data =
            VeilidStream::encode_message(0, 0, my_stream_id, data.into(), self.my_keypair.clone());

        VeilidStream::send_message(api, remote_target, message_data).await;
        debug!("VeilidStream | send_dial");
    }

    pub async fn send_listen(self: &Arc<VeilidStream>) {
        let api = &self.api;
        let remote_target = self.remote_target;
        let my_stream_id = self.my_stream_id;

        let mut data = b"LISTEN".to_vec();
        data.extend_from_slice(&self.my_keypair.public().encode_protobuf());

        let message_data =
            VeilidStream::encode_message(0, 0, my_stream_id, data.into(), self.my_keypair.clone());

        VeilidStream::send_message(api, remote_target, message_data).await;
        debug!("VeilidStream | send_listen");
    }

    pub async fn send_message(api: &Arc<VeilidAPI>, remote_target: Target, message_data: Vec<u8>) {
        trace!(
            "VeilidStream | send_message | {:?}",
            String::from_utf8_lossy(&message_data)
        );
        if let Ok(routing_context) = api.routing_context() {
            let _ = routing_context
                .with_safety(veilid_core::SafetySelection::Unsafe(
                    veilid_core::Sequencing::NoPreference,
                ))
                .unwrap()
                .app_message(remote_target, message_data)
                .await;
        } else {
            error!("VeilidStream | send_ack | could not get routing context");
        }
    }

    pub fn insert_to_outbound_stream(self: &Arc<VeilidStream>, data: &[u8]) {
        trace!("VeilidStream | insert_outbound_stream | start");

        let data_len: u32 = data.len() as u32;

        // Obtain a lock on the read_buffer.
        let mut outbound_stream = self.outbound_stream.lock().unwrap();

        outbound_stream.extend_from_slice(&data_len.to_le_bytes());
        outbound_stream.extend_from_slice(data);

        drop(outbound_stream);

        debug!(
            "VeilidStream | insert_outbound_stream | bytes {:?}",
            data.len()
        );

        trace!(
            "VeilidStream | insert_outbound_stream | bytes {:?} | {:?}",
            data.len(),
            String::from_utf8_lossy(data)
        );

        // Spawn a separate thread to generate and send messages
        if self.is_active() {
            let veilid_stream_clone = Arc::clone(self);
            task::spawn(async move {
                veilid_stream_clone.generate_messages().send_messages();
            });
        }
    }

    pub fn generate_messages(self: &Arc<Self>) -> Arc<Self> {
        trace!("VeilidStream | generate_messages | start");
        let data_limit = SETTINGS.transport_stream_packet_data_size_bytes;

        // Lock the outbound buffer once and keep the lock until we're done.
        let mut stream = self.outbound_stream.lock().unwrap();

        while stream.len() > 0 {
            let mut msg: Vec<u8> = Vec::new();
            let drain_limit = usize::min(data_limit, stream.len());
            let slice: Vec<u8> = stream.drain(..drain_limit).collect();
            msg.extend(slice);

            if msg.len() > 0 {
                let seq = self.increment_outbound_last_seq().get_outbound_last_seq();
                let mut outbound_message_queue = self.outbound_message_queue.lock().unwrap();

                let outbound_msg = OutboundMessage {
                    seq,
                    data: msg,
                    last_sent: None,
                };

                debug!("VeilidStream | generate_messages | seq {:?}", seq);
                outbound_message_queue.insert(seq, outbound_msg);
            }
        }

        self.clone()
    }

    pub fn encode_message(
        received_seq: u32,
        msg_seq: u32,
        stream_id: u64,
        msg_data: Arc<[u8]>,
        keypair: Arc<Keypair>,
    ) -> Vec<u8> {
        debug!("VeilidStream | encode_message");

        let mut buf = Vec::new();
        let signature = keypair.sign(&msg_data);

        match signature {
            Ok(signature) => {
                let payload = Payload {
                    received_seq,
                    msg_seq,
                    stream_id,
                    signature,
                    data: msg_data.to_vec(),
                };

                trace!("VeilidStream | encode_message | payload {:?}", payload);

                match payload.encode(&mut buf) {
                    Ok(_) => {}
                    Err(e) => error!("VeilidStream | encode_message {:?}", e),
                }

                trace!(
                    "VeilidStream | encode_message | {:?} ",
                    String::from_utf8_lossy(&buf)
                );

                trace!("VeilidStream | encode_message | raw {:?} ", msg_data);
            }
            Err(e) => {
                error!("VeilidStream | encode_message {:?}", e)
            }
        }

        buf
    }

    pub fn send_messages(self: &Arc<VeilidStream>) -> Arc<Self> {
        debug!("VeilidStream | send_messages");
        let my_stream_id = self.my_stream_id;

        let resend_interval = Duration::from_secs(SETTINGS.message_retry_timeout);
        let now = Instant::now();

        let messages_to_send: Vec<OutboundMessage>;

        // Acquire lock and collect messages that are ready to be sent
        {
            let mut guard = self.outbound_message_queue.lock().unwrap();
            messages_to_send = guard
                .values()
                .filter(|msg| {
                    let last_sent = msg
                        .last_sent
                        .map_or(now - resend_interval - Duration::new(1, 0), |t| t);
                    now.duration_since(last_sent) >= resend_interval
                })
                .cloned()
                .collect();

            // Update the last_sent timestamp for messages being sent
            for message in &messages_to_send {
                if let Some(outbound_message) = guard.get_mut(&message.seq) {
                    outbound_message.last_sent = Some(now);
                }
            }
        }

        for message in messages_to_send {
            if message.data.len() > SETTINGS.veilid_network_message_limit_bytes {
                panic!("VeilidStream | send_messages | data size is larger than veilid network limits {:?}", SETTINGS.veilid_network_message_limit_bytes)
            }

            // Send logic

            if let Ok(routing_context) = self.api.routing_context() {
                let target = self.remote_target.clone();
                debug!("VeilidStream | send_messages | target {:?}", target);

                let received_seq = self.get_inbound_received_seq();

                let message_data = VeilidStream::encode_message(
                    received_seq,
                    message.seq,
                    my_stream_id,
                    message.data.into(),
                    self.my_keypair.clone(),
                );

                debug!(
                    "VeilidStream | send_messages | message_data len {:?}",
                    message_data.len()
                );

                let stream = self.clone();

                task::spawn(async move {
                    let result = routing_context
                        .with_safety(veilid_core::SafetySelection::Unsafe(
                            veilid_core::Sequencing::NoPreference,
                        ))
                        .unwrap()
                        .app_message(target, message_data.clone())
                        .await;

                    match result {
                        Ok(_) => {
                            stream.update_outbound_last_timestamp_to_now();
                            info!("VeilidStream | send_messages | sent {:?}", message.seq,)
                        }
                        Err(e) => {
                            error!("VeilidStream | send_messages {:?}", e);
                            warn!(
                                "VeilidStream | send_messages | message_data len {:?}",
                                message_data.len()
                            );
                        }
                    }
                });
            }
        }

        self.clone()
    }

    pub fn send_status_if_stale(self: &Arc<VeilidStream>) -> Arc<Self> {
        trace!("VeilidStream | send_status_if_stale");
        let my_stream_id = self.my_stream_id;

        let timeout_duration = Duration::new(SETTINGS.connection_keepalive_timeout, 0);
        let now = Instant::now();
        let last_sent = self.outbound_last_timestamp.lock().unwrap();

        if now.duration_since(*last_sent) >= timeout_duration && self.is_active() {
            debug!("VeilidStream | send_status_if_stale | sending");
            let target = self.remote_target.clone();

            let sent_seq = self.get_outbound_last_seq();
            let received_seq = self.get_inbound_received_seq();
            let data = b"STATUS".to_vec();

            let message_data = VeilidStream::encode_message(
                received_seq,
                sent_seq,
                my_stream_id,
                data.into(),
                self.my_keypair.clone(),
            );

            let veilid_stream_clone = Arc::clone(self);

            task::spawn(async move {
                if let Ok(routing_context) = veilid_stream_clone.api.routing_context() {
                    let result = routing_context
                        .with_safety(veilid_core::SafetySelection::Unsafe(
                            veilid_core::Sequencing::NoPreference,
                        ))
                        .unwrap()
                        .app_message(target, message_data.clone())
                        .await;

                    match result {
                        Ok(_) => {
                            veilid_stream_clone
                                .clone()
                                .update_outbound_last_timestamp_to_now();

                            debug!(
                                "VeilidStream | send_status_if_stale | I have {:?} | I sent {:?}",
                                received_seq, sent_seq
                            )
                        }
                        Err(e) => {
                            error!("VeilidStream | send_status_if_stale {:?}", e);
                            veilid_stream_clone.update_status(StreamStatus::Expired);
                        }
                    }
                }
            });
        }

        self.clone()
    }
}

// #[cfg(test)]
// mod tests {
//     use super::*;
//     use crate::settings::test_config;
//     use crate::VeilidSliceManager;
//     use env_logger;
//     use futures::executor::block_on;
//     use once_cell::sync::OnceCell;
//     use std::sync::Arc;
//     use veilid_core::ConfigCallback;
//     use veilid_core::CryptoKey;
//     use veilid_core::CryptoTyped;
//     use veilid_core::Target;
//     use veilid_core::UpdateCallback;
//     use veilid_core::VeilidAPI;
//     use veilid_core::VeilidUpdate;

//     // #[allow(unused_imports)]
//     // use log::{debug, error, info, trace, warn};

//     extern crate tokio_crate as tokio;

//     static MOCK_API: OnceCell<VeilidAPI> = OnceCell::new();

//     pub fn get_mock_api() -> VeilidAPI {
//         MOCK_API
//             .get_or_init(|| {
//                 let update_cb: UpdateCallback = Arc::new(|_update: VeilidUpdate| {});
//                 let config_cb: ConfigCallback =
//                     Arc::new(move |config_key: String| test_config(&config_key));

//                 let api = block_on(async {
//                     let api_result = veilid_core::api_startup(update_cb, config_cb).await;
//                     api_result.unwrap()
//                 });

//                 api
//             })
//             .clone()
//     }

//     fn get_mock_target() -> Target {
//         let bytes_obj = "Ib9pWCZfmvMYQwW8jt9XS2kCz3KdEgSr".bytes();
//         let bytes_vec: Vec<u8> = bytes_obj.collect();
//         let mut bytes_array = [0u8; 32];
//         bytes_array.copy_from_slice(&bytes_vec);

//         Target::NodeId(CryptoTyped::new(
//             "VLD0".as_bytes().try_into().unwrap(),
//             CryptoKey::new(bytes_array),
//         ))
//     }

//     #[tokio::test]
//     async fn test_insert_outgoing_message() {
//         std::env::set_var("RUST_LOG", "info");
//         env_logger::init();

//         // Arrange: Create a mock VeilidConnection and VeilidMessage
//         let mock_api = get_mock_api();
//         let local_target = get_mock_target();
//         let remote_target = get_mock_target();
//         let mock_connection = VeilidConnection::new(mock_api, local_target, remote_target).unwrap();
//         let mock_message = VeilidMessage::new(mock_connection, Arc::from("mock_data".as_bytes()));

//         // Act: Insert the mock_message
//         let seq_num = VeilidSliceManager::insert_outgoing_message(mock_message);
//         // info!("test_insert_outgoing_message | inserted {:?}", seq_num);

//         // Assert: The message should now be in the outgoing_messages map with the next sequence number
//         let outgoing_messages = VEILID_MESSAGES_MANAGER.outgoing_messages.lock().unwrap();
//         // Further assert that the inserted message is the one we inserted.
//         let inserted_message = outgoing_messages.get(&seq_num).unwrap();

//         assert_eq!(inserted_message.data.as_ref(), "mock_data".as_bytes()); // The data should be what we set
//     }

//     #[tokio::test]
//     async fn test_process_outgoing_messages() {
//         let mock_api = get_mock_api();
//         let local_target = get_mock_target();
//         let remote_target = get_mock_target();
//         let mock_connection =
//             VeilidConnection::new(mock_api.clone(), local_target, remote_target).unwrap();

//         // add two messages
//         let msg = VeilidMessage::new(mock_connection.clone(), Arc::from("data1".as_bytes()));
//         let seq = VeilidSliceManager::insert_outgoing_message(msg);
//         debug!("test_process_outgoing_messages | inserted {:?}", seq);

//         let msg = VeilidMessage::new(mock_connection.clone(), Arc::from("data2".as_bytes()));
//         let seq = VeilidSliceManager::insert_outgoing_message(msg);
//         debug!("test_process_outgoing_messages | inserted {:?}", seq);

//         let outgoing_messages = VEILID_MESSAGES_MANAGER.outgoing_messages.lock().unwrap();
//         assert_eq!(
//             outgoing_messages.len(),
//             2,
//             "There should be exactly two messages in outgoing_messages"
//         );
//         assert!(
//             outgoing_messages.contains_key(&1),
//             "Key 1 should be present in outgoing_messages"
//         );
//         assert!(
//             outgoing_messages.contains_key(&2),
//             "Key 2 should be present in outgoing_messages"
//         );

//         drop(outgoing_messages);

//         // This should send both but say only one was delivered
//         VeilidSliceManager::process_outgoing_messages().await;

//         let outgoing_messages = VEILID_MESSAGES_MANAGER.outgoing_messages.lock().unwrap();
//         // info!("test_process_outgoing_messages 2 {:?}", outgoing_messages);

//         assert!(
//             outgoing_messages.get(&1).is_none(),
//             "Key 1 should be missing from outgoing_messages"
//         );

//         drop(outgoing_messages);

//         let msg = VeilidMessage::new(mock_connection.clone(), Arc::from("data2".as_bytes()));
//         let seq = VeilidSliceManager::insert_outgoing_message(msg);
//         debug!("test_process_outgoing_messages | inserted {:?}", seq);

//         VeilidSliceManager::process_outgoing_messages().await;

//         let outgoing_messages = VEILID_MESSAGES_MANAGER.outgoing_messages.lock().unwrap();

//         assert!(
//             outgoing_messages.get(&2).is_none(),
//             "Key 2 should be missing from outgoing_messages"
//         );
//     }

//     #[tokio::test]
//     async fn test_process_incoming_messages() {
//         // Setup
//         let local_target = get_mock_target();
//         let remote_target = get_mock_target();
//         let mock_api = get_mock_api();
//         let mock_connection =
//             VeilidConnection::new(mock_api.clone(), local_target, remote_target).unwrap();

//         // Insert messages in different order
//         // Message 3
//         let msg3 = VeilidMessage::new(
//             mock_connection.clone(),
//             Arc::from("data3".as_bytes().to_vec().into_boxed_slice()),
//         );

//         VeilidSliceManager::insert_incoming_message(3, msg3);

//         // Message 1
//         let msg1 = VeilidMessage::new(
//             mock_connection.clone(),
//             Arc::from("data1".as_bytes().to_vec().into_boxed_slice()),
//         );

//         VeilidSliceManager::insert_incoming_message(1, msg1);

//         // Message 2
//         let msg2 = VeilidMessage::new(
//             mock_connection.clone(),
//             Arc::from("data2".as_bytes().to_vec().into_boxed_slice()),
//         );

//         VeilidSliceManager::insert_incoming_message(2, msg2);

//         // Act: Call your process_incoming_messages method
//         VeilidSliceManager::process_incoming_messages().await;

//         // Assert: Check if the read_buffer has the expected data
//         let expected_read_buffer: Vec<u8> = [b"data1", b"data2", b"data3"]
//             .iter()
//             .flat_map(|&s| s.iter().copied())
//             .collect();

//         // Assuming mock_connection.read_buffer is publicly accessible
//         let actual_read_buffer = mock_connection.read_buffer.lock().unwrap();

//         assert_eq!(
//             actual_read_buffer.as_slice(),
//             expected_read_buffer.as_slice()
//         );
//     }
// }
