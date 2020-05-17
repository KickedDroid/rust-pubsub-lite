#[macro_use]
extern crate log;

#[path = "../log_util.rs"]
mod log_util;

use std::sync::Arc;

use grpcio::{ChannelBuilder, EnvBuilder};
use grpcio_proto::pubsub::PubSubRequest;
use grpcio_proto::pubsub_grpc::PubSubApiClient;

fn main() {
    let _guard = log_util::init_log(None);
    let env = Arc::new(EnvBuilder::new().build());
    let ch = ChannelBuilder::new(env).connect("localhost:50051");
    let client = PubSubApiClient::new(ch);

    let mut req = PubSubRequest::default();
    req.set_name("world".to_owned());
    let reply = client.say_hello(&req).expect("rpc");
    info!("Greeter received: {}", reply.get_message());
}