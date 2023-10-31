# Libp2p-Transport-Veilid

Enables libp2p-rust apps to communicate node-to-node over Veilid (view Veilid's [launch slides](https://veilid.com/Launch-Slides-Veilid.pdf) from Defcon 2023).

This transport currently supports ONLY direct routes and not Veilid's private / safety routes.

Why use it? Offers [holepunching](https://docs.libp2p.io/concepts/nat/hole-punching/) without operating relay nodes (as this is provided by Veilid).

## Getting Started

### Add the transport to your Cargo.toml

```Cargo.toml
libp2p-transport-veilid = { path = "../packages/libp2p-transport-veilid"}
```

### Add the transport to your libp2p app

```rust
use libp2p_transport_veilid::VeilidTransport;
use tokio::runtime::Handle;
use libp2p::noise::{Keypair, NoiseConfig, X25519Spec},

// the transport uses a handle to run background async processes in stream_handler
let handle = Handle::current();
let cloned_handle = handle.clone();

let keys = identity::Keypair::generate_ed25519();

let auth_keys = Keypair::<X25519Spec>::new()
    .into_authentic(&keys)
    .expect("can create auth keys");

use libp2p::yamux::YamuxConfig;

let transp = VeilidTransport::new(Some(cloned_handle))
    .upgrade(upgrade::Version::V1)
    .authenticate(NoiseConfig::xx(auth_keys).into_authenticated())
    .multiplex(YamuxConfig::default())
    .boxed();

// not shown: behavior and swarm initialization
```

## How It Works

## Settings

Veilid settings are in `./.veilid/`. If `settings` doesn't exist, the file is initialized from default settings from `settings.toml`.

Transport settings are in `Settings` in `lib.rs`.

`./.veilid/` contains the node's keys and other Veilid data structures. These are initilized at startup if they don't exist.

## Key Components

`listener.rs` - VeilidListener receives VeilidUpdate events from the VeilidAPI. This includes incoming data from remote nodes.

`connection.rs`- VeilidConnection represents the connection between the local node and the remote node. Veilid nodes are addressed by `Target` which are modeled like unix domain sockets to fit Libp2p's Multiaddr format `/unix/VLD0:ktJZt5efM1Qd8hxkRVI_NuAp2jOojv2Kkz7R6TxZcAc`.

`stream.rs` - VeilidStream represents the communication channel between the local and a remote node. Libp2p streams are converted into messages and fired off via Veilid's app_message(). The nodes use a sequencing and resync process to ensure undelivered messages are resent. The remote node reconstucts the libp2p stream from the message sequence.

Streams are collected and fetched using VeilidStreamManager.

`stream_handler.rs` - Runs background processes -- resending undelivered messages and cleaning up dead streams.

### More information

[Veilid](https://veilid.com/)
[Rust docs](https://docs.rs/crate/veilid-core/latest)

[rust-libp2p](https://github.com/libp2p/rust-libp2p/)
