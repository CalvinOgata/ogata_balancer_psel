// Code shared between the load balancer and the backend servers.

pub mod parser;
pub mod tls;

pub const SERVER_ID_HEADER: &str = "X-Server-Id";
