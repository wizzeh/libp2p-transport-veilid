use crate::stream::VeilidStream;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;

#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};

// This mod creates various getters and setters to track
// delivery of messages sent and received on a VeilidStream

#[derive(Debug)]
pub struct StreamSeq {
    value: AtomicU32,
}

impl StreamSeq {
    pub fn new(initial_value: u32) -> Self {
        StreamSeq {
            value: AtomicU32::new(initial_value),
        }
    }

    pub fn load(&self) -> u32 {
        let seq = self.value.load(std::sync::atomic::Ordering::SeqCst);
        debug!("StreamSeq | load | seq {:?}", seq);
        seq
    }

    pub fn increment(&self) -> u32 {
        let next_seq_num = self.value.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;

        debug!("StreamSeq | increment | new seq {:?}", next_seq_num);

        next_seq_num
    }

    pub fn update(&self, new_seq: u32) {
        self.value
            .store(new_seq, std::sync::atomic::Ordering::SeqCst)
    }
}

macro_rules! create_getter {
    ($name:ident, $field:ident) => {
        impl VeilidStream {
            pub fn $name(&self) -> u32 {
                trace!(concat!("VeilidStream | ", stringify!($name)));
                self.$field.load()
            }
        }
    };
}

create_getter!(get_outbound_last_seq, outbound_last_seq);
create_getter!(get_outbound_delivered_seq, outbound_delivered_seq);
create_getter!(get_inbound_received_seq, inbound_received_seq);

macro_rules! create_incrementer {
    ($name:ident, $field:ident) => {
        impl VeilidStream {
            pub fn $name(self: &Arc<VeilidStream>) -> Arc<Self> {
                let new_value = self.$field.increment();
                debug!(
                    concat!("VeilidStream | ", stringify!($name), " {:?}"),
                    new_value
                );
                self.clone()
            }
        }
    };
}

create_incrementer!(increment_outbound_last_seq, outbound_last_seq);
// create_incrementer!(increment_outbound_delivered_seq, outbound_delivered_seq);
// create_incrementer!(increment_inbound_received_seq, inbound_received_seq);

macro_rules! create_updater {
    ($name:ident, $field:ident) => {
        impl VeilidStream {
            pub fn $name(self: &Arc<VeilidStream>, new_value: u32) -> Arc<Self> {
                self.$field.update(new_value);

                debug!(
                    concat!("VeilidStream | ", stringify!($name), " {:?}"),
                    new_value
                );

                self.clone()
            }
        }
    };
}

// create_updater!(update_outbound_last_seq, outbound_last_seq);
create_updater!(update_outbound_delivered_seq, outbound_delivered_seq);
create_updater!(update_inbound_received_seq, inbound_received_seq);
