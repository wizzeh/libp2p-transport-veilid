# Libp2p-Transport-Veilid

## To use this transport in a Libp2p app

Add the transport to your Cargo.toml

```Cargo.toml
libp2p-transport-veilid = { path = "../packages/libp2p-transport-veilid"}
```

Add the transport to your libp2p app

```rust
use libp2p_transport_veilid::VeilidTransport;
use tokio::runtime::Handle;
use libp2p::noise::{Keypair, NoiseConfig, X25519Spec},

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
```
