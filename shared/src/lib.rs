// Code shared between the load balancer and the backend servers.
//
// Two concerns live here:
//   * `protocol` — minimal HTTP/1.1 parsing + the small custom types we send
//                  between the load balancer and the servers (health reports).
//   * `tls`      — rustls config helpers so both sides agree on which certs
//                  to trust and which identity to present.

pub mod protocol;
pub mod tls;

pub const SERVER_ID_HEADER: &str = "X-Server-Id";
