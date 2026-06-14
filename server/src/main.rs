// Backend server: accepts mTLS connections and serves PDFs; health reporter runs on a second thread.

use std::env;
use std::io::BufReader;
use std::net::{IpAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConnection, ServerConnection, Stream};
use shared::SERVER_ID_HEADER;
use shared::parser::{Request, read_response, write_request};

mod handler;
mod health;

use handler::{HandlerCtx, handle_connection};
use health::HealthState;

fn main() {
    let cfg = Config::from_env();
    eprintln!(
        "[{}] starting on :{}, files={}, lb={}:{}",
        cfg.server_id,
        cfg.server_port,
        cfg.files_dir.display(),
        cfg.lb_host,
        cfg.lb_health_port
    );

    let tls_server =
        shared::tls::server_config_mtls(&cfg.cert_path, &cfg.key_path, &cfg.lb_client_ca_path)
            .expect("failed to build mTLS server config");
    let tls_client =
        shared::tls::client_config(&cfg.cert_path).expect("failed to build TLS client config");

    let health = HealthState::new(cfg.server_id.clone());

    {
        let cfg = cfg.clone();
        let tls_client = tls_client.clone();
        let health = health.clone();
        thread::Builder::new()
            .name("health-reporter".into())
            .spawn(move || run_health_reporter(cfg, tls_client, health))
            .expect("spawn health reporter");
    }

    let ctx = Arc::new(HandlerCtx {
        files_dir: cfg.files_dir.clone(),
        health: health.clone(),
    });

    let listener =
        TcpListener::bind(("0.0.0.0", cfg.server_port)).expect("failed to bind server port");

    for incoming in listener.incoming() {
        let mut sock = match incoming {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[{}] accept error: {}", cfg.server_id, e);
                continue;
            }
        };

        // IP allowlist — checked before TLS, before reading any bytes.
        let peer_ip = match sock.peer_addr() {
            Ok(a) => a.ip(),
            Err(e) => {
                eprintln!(
                    "[{}] could not read peer address, rejecting: {}",
                    cfg.server_id, e
                );
                continue;
            }
        };
        if !cfg.lb_addrs.contains(&peer_ip) {
            eprintln!(
                "[{}] rejected {} — not the load balancer",
                cfg.server_id, peer_ip
            );
            continue;
        }

        let _ = sock.set_read_timeout(Some(Duration::from_secs(30)));
        let _ = sock.set_write_timeout(Some(Duration::from_secs(30)));

        let ctx = ctx.clone();
        let tls_server = tls_server.clone();
        thread::spawn(move || {
            let mut tls = match ServerConnection::new(tls_server) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!(
                        "[{}] tls handshake setup failed: {}",
                        ctx.health.server_id, e
                    );
                    return;
                }
            };
            handle_connection(&ctx, &mut tls, &mut sock, peer_ip);
            handler::close_tls(&mut tls, &mut sock);
        });
    }
}

#[derive(Clone)]
struct Config {
    server_id: String,
    server_port: u16,
    files_dir: PathBuf,
    cert_path: PathBuf,
    key_path: PathBuf,
    lb_host: String,
    lb_health_port: u16,
    lb_addrs: Vec<IpAddr>,
    lb_client_ca_path: PathBuf,
    report_interval: Duration,
}

impl Config {
    fn from_env() -> Self {
        let server_id = env::var("SERVER_ID").unwrap_or_else(|_| "server-unset".into());
        let server_port: u16 = env::var("SERVER_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4443);
        let files_dir =
            PathBuf::from(env::var("FILES_DIR").unwrap_or_else(|_| "/app/files".into()));
        let cert_path =
            PathBuf::from(env::var("CERT_PATH").unwrap_or_else(|_| "/app/certs/cert.pem".into()));
        let key_path =
            PathBuf::from(env::var("KEY_PATH").unwrap_or_else(|_| "/app/certs/key.pem".into()));
        let lb_host = env::var("LB_HEALTH_INGEST_HOST").unwrap_or_else(|_| "load_balancer".into());
        let lb_health_port = env::var("LB_HEALTH_INGEST_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(9443);
        let report_interval = Duration::from_secs(
            env::var("HEALTH_REPORT_INTERVAL_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(2),
        );

        // Retry loop: Docker DNS may not resolve the LB hostname immediately at startup.
        let lb_addrs: Vec<IpAddr> = {
            let addr_str = format!("{}:{}", lb_host, lb_health_port);
            let mut attempts = 0u32;
            loop {
                match addr_str.to_socket_addrs() {
                    Ok(iter) => {
                        let ips: Vec<IpAddr> = iter.map(|a| a.ip()).collect();
                        if !ips.is_empty() {
                            break ips;
                        }
                    }
                    Err(_) => {}
                }
                attempts += 1;
                if attempts >= 30 {
                    panic!(
                        "cannot resolve load balancer '{}' after {} attempts",
                        lb_host, attempts
                    );
                }
                thread::sleep(Duration::from_secs(1));
            }
        };

        let lb_client_ca_path = PathBuf::from(
            env::var("LB_CLIENT_CA_PATH").unwrap_or_else(|_| "/app/certs/lb-client.pem".into()),
        );

        Self {
            server_id,
            server_port,
            files_dir,
            cert_path,
            key_path,
            lb_host,
            lb_health_port,
            lb_addrs,
            lb_client_ca_path,
            report_interval,
        }
    }
}

fn run_health_reporter(
    cfg: Config,
    tls_client: Arc<rustls::ClientConfig>,
    health: Arc<HealthState>,
) {
    loop {
        thread::sleep(cfg.report_interval);
        if let Err(e) = push_one_report(&cfg, &tls_client, &health) {
            eprintln!("[{}] health report failed: {}", cfg.server_id, e);
        }
    }
}

fn push_one_report(
    cfg: &Config,
    tls_client: &Arc<rustls::ClientConfig>,
    health: &Arc<HealthState>,
) -> std::io::Result<()> {
    let report = health.snapshot();
    let body = report.encode().into_bytes();

    let server_name = ServerName::try_from(cfg.lb_host.clone())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;
    let mut tls = ClientConnection::new(tls_client.clone(), server_name)
        .map_err(|e| std::io::Error::other(e.to_string()))?;

    let addr = format!("{}:{}", cfg.lb_host, cfg.lb_health_port);
    let mut sock =
        TcpStream::connect_timeout(&addr.to_socket_addrs_first()?, Duration::from_secs(5))?;
    sock.set_read_timeout(Some(Duration::from_secs(5)))?;
    sock.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut stream = Stream::new(&mut tls, &mut sock);

    let req = Request {
        method: "POST".into(),
        target: "/_health".into(),
        version: "HTTP/1.1".into(),
        headers: vec![
            ("Host".into(), cfg.lb_host.clone()),
            ("Content-Type".into(), "text/plain; charset=utf-8".into()),
            ("Content-Length".into(), body.len().to_string()),
            ("Connection".into(), "close".into()),
            (SERVER_ID_HEADER.into(), cfg.server_id.clone()),
        ],
        body,
    };
    write_request(&mut stream, &req)?;

    let mut reader = BufReader::new(&mut stream);
    match read_response(&mut reader) {
        Ok(resp) if resp.status / 100 == 2 => {}
        Ok(resp) => {
            eprintln!(
                "[{}] LB rejected health report: {} {}",
                cfg.server_id, resp.status, resp.reason,
            );
        }
        Err(e) => {
            if e.kind() != std::io::ErrorKind::UnexpectedEof {
                return Err(e);
            }
        }
    }
    Ok(())
}

trait ToSocketAddrsFirst {
    fn to_socket_addrs_first(&self) -> std::io::Result<std::net::SocketAddr>;
}

impl ToSocketAddrsFirst for String {
    fn to_socket_addrs_first(&self) -> std::io::Result<std::net::SocketAddr> {
        use std::net::ToSocketAddrs;
        self.to_socket_addrs()?
            .next()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, "no addrs"))
    }
}
