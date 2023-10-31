use lazy_static::lazy_static;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use std::task::{Context, Waker};
use tokio_crate::task;
use tokio_crate::time::{Duration, Instant};
use veilid_core::{Target, VeilidAPI};

#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};

use crate::stream_seq::StreamSeq;
use crate::SETTINGS;

// This mod creates an inbound and outbound stream of data between
// the local and remote nodes. Processes fetch a stream by its remote target
// using VeilidStreamManager

pub enum StreamStatus {
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
    // the address of the remote Veilid node
    remote_target: Target,
    // handle to wake AsyncRead for VeilidConnection
    waker: Arc<Mutex<Option<Waker>>>,
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
}

impl VeilidStream {
    pub fn new(api: Arc<VeilidAPI>, remote_target: Target) {
        let stream = Self {
            api,
            remote_target,

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
        };
        VeilidStreamManager::insert_stream(stream.into());
    }

    pub fn update_inbound_last_timestamp_to_now(self: Arc<VeilidStream>) -> Arc<Self> {
        debug!("VeilidStream | update_inbound_last_timestamp_to_now");

        let mut last_active = self.inbound_last_timestamp.lock().unwrap();
        *last_active = Instant::now();
        self.clone()
    }

    pub fn update_outbound_last_timestamp_to_now(self: Arc<VeilidStream>) -> Arc<Self> {
        debug!("VeilidStream | update_outbound_last_timestamp_to_now");

        let mut last_active = self.outbound_last_timestamp.lock().unwrap();
        *last_active = Instant::now();
        self.clone()
    }

    pub async fn is_active(&self) -> bool {
        debug!("VeilidStream | is_active");
        // the stream is active if we've received a message within the timeout deadline
        let seconds = SETTINGS.stream_timeout_secs;
        let last_active = self.inbound_last_timestamp.lock().unwrap();
        *last_active > Instant::now() - Duration::new(seconds, 0)
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

    // Inbound
    pub fn decode_message(packet: &[u8]) -> Result<(u32, u32, Vec<u8>), &'static str> {
        trace!("VeilidStream | decode_message");

        if packet.len() < 4 {
            return Err("Packet too short to contain a u32 sequence number");
        }

        let delivered_seq = u32::from_le_bytes([packet[0], packet[1], packet[2], packet[3]]);

        let seq = u32::from_le_bytes([packet[4], packet[5], packet[6], packet[7]]);

        let data = packet[8..].to_vec();

        trace!(
            "VeilidStream | decode_message | they have {:?} | {:?} bytes {:?}",
            delivered_seq,
            seq,
            data.len()
        );

        Ok((delivered_seq, seq, data))
    }

    pub async fn recv_message(
        self: Arc<VeilidStream>,
        delivered_seq: u32,
        seq: u32,
        data: Vec<u8>,
    ) -> StreamStatus {
        if !self.is_active().await {
            return StreamStatus::Expired;
        }

        trace!(
            "VeilidStream | recv_message | seq {:?} bytes {:?}",
            seq,
            data.len()
        );

        // check if we need this payload
        let inbound_received_seq = self.get_inbound_received_seq();
        let mut final_seq = inbound_received_seq;

        if seq <= inbound_received_seq {
            // discard duplicates
            debug!("VeilidStream | recv_message | duplicate");
        } else if seq > inbound_received_seq + 1 {
            // store it for later
            debug!("VeilidStream | recv_message | store for later {:?}", seq);

            let result = self.inbound_message_queue.lock();
            match result {
                Ok(mut queue) => {
                    queue.insert(seq, data);
                }
                Err(e) => {
                    error!("VeilidStream | recv_message {:?}", e);
                    return StreamStatus::Expired;
                }
            }
        } else {
            // receive it
            self.recv_inbound_buffer(&data);

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
                    return StreamStatus::Expired;
                }
            }
        };

        // update stream
        self.update_inbound_received_seq(final_seq)
            .update_outbound_delivered_seq(delivered_seq)
            .remove_sent_messages_from_queue()
            .update_inbound_last_timestamp_to_now()
            .update_inbound_stream();
        // .set();

        return StreamStatus::Active;
    }

    pub fn recv_inbound_buffer(&self, data: &[u8]) {
        trace!("VeilidStream | recv_inbound_buffer | start");

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

            trace!(
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

    pub fn read_inbound_stream(&self, cx: &mut Context<'_>, buf: &mut [u8]) -> Option<usize> {
        let mut stream = self.inbound_stream.lock().unwrap();
        trace!(
            "VeilidStream | read_inbound_stream | stream {:?}",
            String::from_utf8_lossy(&stream)
        );
        if stream.is_empty() {
            trace!("VeilidStream | read_inbound_stream | empty");
            *self.waker.lock().unwrap() = Some(cx.waker().clone());
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
        trace!("VeilidStream | remove_sent_messages_from_queue");

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
        let veilid_stream_clone = Arc::clone(self);
        task::spawn(async move {
            veilid_stream_clone
                .generate_messages(SETTINGS.transport_stream_packet_data_size_bytes)
                .send_app_msg()
                .await;
        });
    }

    pub fn generate_messages(self: &Arc<Self>, buf_min: usize) -> Arc<Self> {
        trace!("VeilidStream | generate_messages | start");
        let data_limit = SETTINGS.transport_stream_packet_data_size_bytes;

        // Lock the outbound buffer once and keep the lock until we're done.
        let mut stream = self.outbound_stream.lock().unwrap();

        while stream.len() > buf_min {
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

    pub fn encode_message(received_seq: u32, msg_seq: u32, msg_data: Arc<[u8]>) -> Vec<u8> {
        trace!("VeilidStream | encode_message");

        let mut packet = Vec::new();

        // Convert sequence number to little-endian byte array and add local's received seq number
        packet.extend_from_slice(&received_seq.to_le_bytes());

        // add this messages seq
        packet.extend_from_slice(&msg_seq.to_le_bytes());

        // add the msg data slice
        packet.extend_from_slice(&msg_data);

        trace!(
            "VeilidStream | encode_message | {:?} ",
            String::from_utf8_lossy(&packet)
        );

        trace!("VeilidStream | encode_message | raw {:?} ", msg_data);

        packet
    }

    pub async fn send_app_msg(self: &Arc<VeilidStream>) -> Arc<Self> {
        trace!("VeilidStream | get_outbound_delivered_seq");

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
                panic!("VeilidStream | send_app_msg | data size is larger than veilid network limits {:?}", SETTINGS.veilid_network_message_limit_bytes)
            }

            // Send logic
            let routing_context = self.api.routing_context();

            // let routing_context = self.api.routing_context();
            // let routing_context = self
            //     .api
            //     .routing_context()
            //     .with_sequencing(Sequencing::EnsureOrdered);

            let target = self.remote_target.clone();

            let received_seq = self.get_inbound_received_seq();
            let message_data =
                VeilidStream::encode_message(received_seq, message.seq, message.data.into());

            let result = routing_context
                .app_message(target, message_data.clone())
                .await;
            match result {
                Ok(_) => {
                    self.clone().update_outbound_last_timestamp_to_now();

                    debug!(
                        "VeilidStream | send_app_msg | I have {:?} | sending {:?}",
                        received_seq, message.seq,
                    )
                }
                Err(e) => warn!("VeilidStream | send_app_msg {:?}", e),
            }
        }

        self.clone()
    }

    pub async fn send_status_if_stale(self: &Arc<VeilidStream>) -> Arc<Self> {
        trace!("VeilidStream | send_status_if_stale");

        let timeout_duration = Duration::new(SETTINGS.connection_keepalive_timeout, 0);
        let now = Instant::now();
        let last_sent = self.outbound_last_timestamp.lock().unwrap();

        if now.duration_since(*last_sent) >= timeout_duration {
            let target = self.remote_target.clone();

            let sent_seq = self.get_outbound_last_seq();
            let received_seq = self.get_inbound_received_seq();
            let data = b"STATUS".to_vec();

            let message_data = VeilidStream::encode_message(received_seq, sent_seq, data.into());

            let veilid_stream_clone = Arc::clone(self);
            task::spawn(async move {
                let routing_context = veilid_stream_clone.api.routing_context();

                let result = routing_context
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
                    Err(e) => warn!("VeilidStream | send_app_msg {:?}", e),
                }
            });
        }

        self.clone()
    }
}

pub struct VeilidStreamManager {
    streams: Arc<Mutex<HashMap<TargetWrapper, Arc<VeilidStream>>>>,
}

lazy_static! {
    pub static ref VEILID_STREAM_MANAGER: VeilidStreamManager = {
        VeilidStreamManager {
            streams: Arc::new(Mutex::new(HashMap::new())),
        }
    };
}

impl VeilidStreamManager {
    pub fn insert_stream(stream: Arc<VeilidStream>) {
        debug!("VeilidStreamManager | insert_stream");
        let target = stream.remote_target.clone();
        let wrapped_target = TargetWrapper(target);
        let mut streams = VEILID_STREAM_MANAGER.streams.lock().unwrap();
        streams.insert(wrapped_target, stream);
    }

    pub fn remove_stream(stream: Arc<VeilidStream>) {
        let target = stream.remote_target.clone();
        let wrapped_target = TargetWrapper(target);
        let mut streams = VEILID_STREAM_MANAGER.streams.lock().unwrap();
        streams.remove(&wrapped_target);
    }

    pub fn get_stream(target: &Target) -> Option<Arc<VeilidStream>> {
        let wrapped_target = TargetWrapper(target.clone());
        let streams = VEILID_STREAM_MANAGER.streams.lock().unwrap();
        streams.get(&wrapped_target).cloned()
    }

    pub async fn get_all() -> Vec<Arc<VeilidStream>> {
        // Acquire a lock on the streams collection
        let streams = VEILID_STREAM_MANAGER.streams.lock().unwrap();

        // Collect all VeilidStream references into a vector and return it
        streams.values().cloned().collect()
    }

    pub async fn clean() {
        let now = Instant::now();
        let timeout_duration = Duration::new(SETTINGS.stream_timeout_secs, 0);

        // Acquire a lock on the streams collection
        let mut streams = VEILID_STREAM_MANAGER.streams.lock().unwrap();
        let old = streams.len();

        // Retain only the streams that have not exceeded the timeout
        streams.retain(|_, stream| {
            // Acquire a lock on the last_active_timestamp field of the stream
            let last_active = stream.inbound_last_timestamp.lock().unwrap();

            // Compare the last active timestamp to the current time and timeout duration
            now.duration_since(*last_active) <= timeout_duration
        });
        let new = streams.len();
        if new < old {
            info!(
                "VeilidStreamManager | clean | removed streams {:?}",
                old - new
            )
        }
    }
}

struct TargetWrapper(Target);

impl PartialEq for TargetWrapper {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Target::NodeId(key1), Target::NodeId(key2)) => key1 == key2,
            (Target::PrivateRoute(route_id1), Target::PrivateRoute(route_id2)) => {
                route_id1 == route_id2
            }
            _ => false,
        }
    }
}

impl Eq for TargetWrapper {}

impl std::hash::Hash for TargetWrapper {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match &self.0 {
            Target::NodeId(key) => {
                "NodeId".hash(state);
                key.hash(state);
            }
            Target::PrivateRoute(route_id) => {
                "PrivateRoute".hash(state);
                route_id.hash(state);
            }
        }
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
//         trace!("test_process_outgoing_messages | inserted {:?}", seq);

//         let msg = VeilidMessage::new(mock_connection.clone(), Arc::from("data2".as_bytes()));
//         let seq = VeilidSliceManager::insert_outgoing_message(msg);
//         trace!("test_process_outgoing_messages | inserted {:?}", seq);

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
//         trace!("test_process_outgoing_messages | inserted {:?}", seq);

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
