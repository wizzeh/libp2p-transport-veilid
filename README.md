# Libp2p-Transport-Veilid

This is ALPHA software. Use at own risk!

Enables libp2p-rust apps to communicate node-to-node over Veilid (view Veilid's [launch slides](https://veilid.com/Launch-Slides-Veilid.pdf) from Defcon 2023).

This transport currently supports both direct routes and Veilid's private / safety routes which hides the sender and receipient's IP address from each other using [private routing](https://veilid.com/docs/private-routing/).

[Connect with us - join our SimpleX group](https://simplex.chat/contact#/?v=2-5&smp=smp%3A%2F%2FSkIkI6EPd2D63F4xFKfHk7I1UGZVNn6k1QWZ5rcyr6w%3D%40smp9.simplex.im%2FSWxE-5AB6wXyqvx94xB_aelg7Kw3SCjd%23%2F%3Fv%3D1-2%26dh%3DMCowBQYDK2VuAyEAXEixok41ekdED6xD0iia9NiZO3F0BbJ1ygKGTh6ROGs%253D%26srv%3Djssqzccmrcws6bhmn77vgmhfjmhwlyr3u7puw4erkyoosywgl67slqqd.onion&data=%7B%22type%22%3A%22group%22%2C%22groupLinkId%22%3A%22Lke8fQgDE3km91u7qth7dA%3D%3D%22%7D)

## Getting Started

### Check out Tickle, the example app

[Tickle](https://codeberg.org/ffffff_rabbit/tickle) is an R&D app written to demonstrate the use of libp2p-transport-veilid.

### Add the transport to your Cargo.toml

```Cargo.toml
libp2p-transport-veilid = { path = "../packages/libp2p-transport-veilid"}
```

### Add the transport to your libp2p app

Note: The example below is not how the Tickle app configures the transport. Tickle has a transport configuration switch so you can play around with both TCP and Veilid. See the current Tickle main.rs file.  

```rust
use libp2p_transport_veilid::VeilidTransport;
use tokio::runtime::Handle;
use libp2p::noise::{Keypair, NoiseConfig, X25519Spec},

// the transport uses a handle to run background async processes in stream_handler
let handle = Handle::current();
let cloned_handle = handle.clone();

let node_keys = identity::Keypair::generate_ed25519();

let auth_keys = Keypair::<X25519Spec>::new()
    .into_authentic(&node_keys)
    .expect("can create auth keys");

use libp2p::yamux::YamuxConfig;

// Choose from VeilidTransportType::Safe (with Veilid's private routing for IP privacy) 
// or VeilidTransportType::Unsafe (direct connections)
VeilidTransport::new(Some(cloned_handle), node_keys.clone(), VeilidTransportType::Safe)
    .upgrade(upgrade::Version::V1)
    .authenticate(NoiseConfig::xx(auth_keys).into_authenticated())
    .multiplex(YamuxConfig::default())
    .boxed();

// not shown: behavior and swarm initialization
```

## How It Works

## Settings

Veilid settings are in `./.veilid/{node_id}/settings`. If `settings` doesn't exist, the file is initialized from default settings from `settings.toml`. Node_id is generated from the keypair so that multiple instances of Veilid can run on a device (for testing purposes).

Transport settings are in `Settings` struct in `lib.rs`.

`./.veilid/{node_id}` contains the node's keys and other Veilid data structures. These are initilized at startup if they don't exist.

## Key Components

`address.rs` - Address coordinates between Libp2p's Multiaddr format and Veilid's private routes (for VeilidSafe) and node ids (for VeilidUnsafe).

`listener.rs` - VeilidListener receives VeilidUpdate events from VeilidAPI. This includes incoming data from remote nodes. This struct contains the set of active streams.

`connection.rs`- VeilidConnection represents the connection between the local node and the remote node. Veilid nodes are addressed by `Target` which are modeled in Libp2p like unix domain sockets to fit Libp2p's Multiaddr format `/unix/VLD0:ktJZt5efM1Qd8hxkRVI_NuAp2jOojv2Kkz7R6TxZcAc`. The connection struct has the active stream so that it can read and write data.

`stream.rs` - VeilidStream represents the communication channel between the local and a remote node. Libp2p streams are converted into messages and fired off via Veilid's app_message(). The nodes use a sequencing and resync process to ensure undelivered messages are resent. The remote node reconstucts the libp2p stream from the message sequence.

### More information

About Veilid

[Veilid](https://veilid.com/)
[Rust docs](https://docs.rs/crate/veilid-core/latest)

About Libp2p

[rust-libp2p](https://github.com/libp2p/rust-libp2p/)
