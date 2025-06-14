use std::net::SocketAddr;

#[derive(Clone)]
pub struct Config {
    pub listen_addr: SocketAddr,
    pub balena_api_endpoint: String,
    pub legacy_supervisor_url: String,
}
