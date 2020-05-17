use async_std::{io, task};
use futures::{future, prelude::*};
use libp2p::{
    core::{either::EitherTransport, transport::upgrade::Version, StreamMuxer},
    gossipsub::{self, Gossipsub, GossipsubConfigBuilder, GossipsubEvent},
    identify::{Identify, IdentifyEvent},
    identity,
    multiaddr::Protocol,
    ping::{self, Ping, PingConfig, PingEvent},
    pnet::{PnetConfig, PreSharedKey},
    secio::SecioConfig,
    swarm::NetworkBehaviourEventProcess,
    tcp::TcpConfig,
    yamux::Config as YamuxConfig,
    Multiaddr, NetworkBehaviour, PeerId, Swarm, Transport,
};
use std::{
    env,
    error::Error,
    fs,
    path::Path,
    str::FromStr,
    task::{Context, Poll},
    time::Duration,
};

/// Builds the transport that serves as a common ground for all connections.
pub fn build_transport(
    key_pair: identity::Keypair,
    psk: Option<PreSharedKey>,
) -> impl Transport<
    Output = (
        PeerId,
        impl StreamMuxer<
                OutboundSubstream = impl Send,
                Substream = impl Send,
                Error = impl Into<io::Error>,
            > + Send
            + Sync,
    ),
    Error = impl Error + Send,
    Listener = impl Send,
    Dial = impl Send,
    ListenerUpgrade = impl Send,
> + Clone {
    let secio_config = SecioConfig::new(key_pair);
    let yamux_config = YamuxConfig::default();

    let base_transport = TcpConfig::new().nodelay(true);
    let maybe_encrypted = match psk {
        Some(psk) => EitherTransport::Left(
            base_transport.and_then(move |socket, _| PnetConfig::new(psk).handshake(socket)),
        ),
        None => EitherTransport::Right(base_transport),
    };
    maybe_encrypted
        .upgrade(Version::V1)
        .authenticate(secio_config)
        .multiplex(yamux_config)
        .timeout(Duration::from_secs(20))
}

/// Get the current ipfs repo path, either from the IPFS_PATH environment variable or
/// from the default $HOME/.ipfs
fn get_ipfs_path() -> Box<Path> {
    env::var("IPFS_PATH")
        .map(|ipfs_path| Path::new(&ipfs_path).into())
        .unwrap_or_else(|_| {
            env::var("HOME")
                .map(|home| Path::new(&home).join(".ipfs"))
                .expect("could not determine home directory")
                .into()
        })
}

/// Read the pre shared key file from the given ipfs directory
fn get_psk(path: Box<Path>) -> std::io::Result<Option<String>> {
    let swarm_key_file = path.join("swarm.key");
    match fs::read_to_string(swarm_key_file) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// for a multiaddr that ends with a peer id, this strips this suffix. Rust-libp2p
/// only supports dialing to an address without providing the peer id.
fn strip_peer_id(addr: &mut Multiaddr) {
    let last = addr.pop();
    match last {
        Some(Protocol::P2p(peer_id)) => {
            let mut addr = Multiaddr::empty();
            addr.push(Protocol::P2p(peer_id));
            println!(
                "removing peer id {} so this address can be dialed by rust-libp2p",
                addr
            );
        }
        Some(other) => addr.push(other),
        _ => {}
    }
}

/// parse a legacy multiaddr (replace ipfs with p2p), and strip the peer id
/// so it can be dialed by rust-libp2p
fn parse_legacy_multiaddr(text: &str) -> Result<Multiaddr, Box<dyn Error>> {
    let sanitized = text
        .split('/')
        .map(|part| if part == "ipfs" { "p2p" } else { part })
        .collect::<Vec<_>>()
        .join("/");
    let mut res = Multiaddr::from_str(&sanitized)?;
    strip_peer_id(&mut res);
    Ok(res)
}

fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    let ipfs_path: Box<Path> = get_ipfs_path();
    println!("using IPFS_PATH {:?}", ipfs_path);
    let psk: Option<PreSharedKey> = get_psk(ipfs_path)?
        .map(|text| PreSharedKey::from_str(&text))
        .transpose()?;

    // Create a random PeerId
    let local_key = identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(local_key.public());
    println!("using random peer id: {:?}", local_peer_id);
    for psk in psk {
        println!("using swarm key with fingerprint: {}", psk.fingerprint());
    }

    // Set up a an encrypted DNS-enabled TCP Transport over and Yamux protocol
    let transport = build_transport(local_key.clone(), psk);

    // Create a Gosspipsub topic
    let gossipsub_topic = gossipsub::Topic::new("chat".into());
    //let gossipsub_topic2 = gossipsub::Topic::new("c".into());

    // We create a custom network behaviour that combines gossipsub, ping and identify.
    #[derive(NetworkBehaviour)]
    struct MyBehaviour {
        gossipsub: Gossipsub,
        identify: Identify,
        ping: Ping,
    }

    impl NetworkBehaviourEventProcess<IdentifyEvent>
        for MyBehaviour
    {
        // Called when `identify` produces an event.
        fn inject_event(&mut self, event: IdentifyEvent) {
            println!("identify: {:?}", event);
        }
    }

    impl NetworkBehaviourEventProcess<GossipsubEvent>
        for MyBehaviour
    {
        // Called when `gossipsub` produces an event.
        fn inject_event(&mut self, event: GossipsubEvent) {
            match event {
                GossipsubEvent::Message(peer_id, id, message) => {
                    println!(
                        "Got message: {} with id: {} from peer: {:?}",
                        String::from_utf8_lossy(&message.data),
                        id,
                        peer_id
                    )
                }           
                _ => {}
            }
        }
    }

    impl NetworkBehaviourEventProcess<PingEvent>
        for MyBehaviour
    {
        // Called when `ping` produces an event.
        fn inject_event(&mut self, event: PingEvent) {
            use ping::handler::{PingFailure, PingSuccess};
            match event {
                PingEvent {
                    peer,
                    result: Result::Ok(PingSuccess::Ping { rtt }),
                } => {
                    
                }
                PingEvent {
                    peer,
                    result: Result::Ok(PingSuccess::Pong),
                } => {
                    println!("ping: pong from {}", peer.to_base58());
                }
                PingEvent {
                    peer,
                    result: Result::Err(PingFailure::Timeout),
                } => {
                    println!("ping: timeout to {}", peer.to_base58());
                }
                PingEvent {
                    peer,
                    result: Result::Err(PingFailure::Other { error }),
                } => {
                    println!("ping: failure with {}: {}", peer.to_base58(), error);
                }
            }
        }
    }

    // Create a Swarm to manage peers and events
    let mut swarm = {
        let gossipsub_config = GossipsubConfigBuilder::default()
            .max_transmit_size(262144)
            .build();
        let mut behaviour = MyBehaviour {
            gossipsub: Gossipsub::new(local_peer_id.clone(), gossipsub_config),
            identify: Identify::new(
                "/ipfs/0.1.0".into(),
                "rust-ipfs-example".into(),
                local_key.public(),
            ),
            ping: Ping::new(PingConfig::new()),
        };

        println!("Subscribing to {:?}", gossipsub_topic);
        behaviour.gossipsub.subscribe(gossipsub_topic.clone());
        Swarm::new(transport, behaviour, local_peer_id.clone())
    };

    // Reach out to other nodes if specified
    for to_dial in std::env::args().skip(1) {
        let addr: Multiaddr = parse_legacy_multiaddr(&to_dial)?;
        Swarm::dial_addr(&mut swarm, addr)?;
        println!("Dialed {:?}", to_dial)
    }

    // Read full lines from stdin
    let mut stdin = io::BufReader::new(io::stdin()).lines();

    // Listen on all interfaces and whatever port the OS assigns
    Swarm::listen_on(&mut swarm, "/ip4/0.0.0.0/tcp/0".parse()?)?;

    // Kick it off
    let mut listening = false;
    task::block_on(future::poll_fn(move |cx: &mut Context| {
        

        loop {
            match stdin.try_poll_next_unpin(cx)? {
                Poll::Ready(Some(line)) => handle_input_line(&mut swarm.gossipsub, line),
                Poll::Ready(None) => panic!("Stdin closed"),
                Poll::Pending => break
            }
        }
        loop {
            match swarm.poll_next_unpin(cx) {
                Poll::Ready(Some(event)) => println!("{:?}", event),
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => {
                    if !listening {
                        for addr in Swarm::listeners(&swarm) {
                            println!("Address {}/ipfs/{}", addr, local_peer_id);
                            listening = true;
                        }
                    }
                    break;
                }
            }
        }
        Poll::Pending
    }))
}

fn handle_input_line(gossipsub: &mut Gossipsub, line: String) {
    let mut args = line.split(" ");

    match args.next() {
        Some("SUB") => {
            let topic = {
                match args.next() {
                    Some(topic) => gossipsub::Topic::new(String::from(topic).into()),
                    None => {
                        eprintln!("Expected topic");
                        return;
                    }
                }
            };
            let x = gossipsub.subscribe(topic.clone());
            if x == true {
                println!("Subscribed to topic {:?}", topic);
            } else {
                println!("Failed to subscribe to topic");
            }
        }
        Some("PUB") => {
            let topic = {
                match args.next() {
                    Some(topic) => gossipsub::Topic::new(topic.into()),
                    None => {
                        eprintln!("Expected topic");
                        return;
                    }
                }
            };
            let msg = {
                match args.next() {
                    Some(msg) => msg,
                    None => {
                        eprintln!("Expected message");
                        return;
                    }
                }
            };
            gossipsub.publish(&topic.clone(), msg.as_bytes());
        }
        _ => {
            eprintln!("expected PUB or SUB");
        }
    }
}