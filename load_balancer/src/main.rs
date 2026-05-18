// Load balancer entry point.
//
// Two TLS listeners:
//   * `public_port`        — clients (browsers, curl) hit this. Frontend
//                            assets are served directly; everything else is
//                            proxied to a backend over mTLS.
//   * `health_ingest_port` — backend servers POST `/_health` here. Separate
//                            socket so client traffic and control-plane
//                            traffic can never get crossed.
//
// Each connection is handled on its own std::thread.

use std::env;
use std::io::BufReader;
use std::net::{IpAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rustls::{ServerConnection, Stream};
use shared::protocol::{read_request, write_response};

mod health;
mod proxy;
mod router;
mod scheduler;

use health::{Backend, HealthCtx, Registry};
use proxy::ProxyCtx;
use router::RouterCtx;

// LB process entry point. Boots the registry, spawns the health-ingest
// listener and the plain-HTTP listener, then blocks on the public TLS
// listener which accepts client traffic.
fn main() {
    let cfg = Config::from_env();
    eprintln!(
        "load_balancer starting: https=:{}, http=:{}, health=:{}, backends={:?}",
        cfg.public_port, cfg.public_http_port, cfg.health_ingest_port, cfg.backend_hosts,
    );

    let tls_server = shared::tls::server_config(&cfg.cert_path, &cfg.key_path)
        .expect("failed to build TLS server config");
    let tls_client = shared::tls::client_config_mtls(
        &cfg.cert_path,
        &cfg.lb_client_cert_path,
        &cfg.lb_client_key_path,
    )
    .expect("failed to build mTLS client config");

    let seeds: Vec<Backend> = cfg
        .backend_hosts
        .iter()
        .map(|host| Backend::new(host.clone(), host.clone(), cfg.backend_port))
        .collect();
    let registry = Arc::new(Registry::with_seed(seeds));

    // Health ingest listener — backend -> LB.
    {
        let registry = registry.clone();
        let tls_server = tls_server.clone();
        let port = cfg.health_ingest_port;
        thread::Builder::new()
            .name("health-ingest".into())
            .spawn(move || run_health_listener(port, tls_server, registry))
            .expect("spawn health listener");
    }

    // Public listener — clients -> LB.
    let router_ctx = Arc::new(RouterCtx {
        registry: registry.clone(),
        proxy: ProxyCtx { tls_client },
        frontend_dir: cfg.frontend_dir.clone(),
    });

    // Plain-HTTP listener — browser convenience without TLS cert acceptance.
    // Skipped if `LB_HTTP_PORT=0`.
    if cfg.public_http_port != 0 {
        let router_ctx = router_ctx.clone();
        let port = cfg.public_http_port;
        thread::Builder::new()
            .name("public-http".into())
            .spawn(move || run_public_http_listener(port, router_ctx))
            .expect("spawn public http listener");
    }

    run_public_listener(cfg.public_port, tls_server, router_ctx);
}

#[derive(Clone)]
struct Config {
    public_port: u16,
    public_http_port: u16,
    health_ingest_port: u16,
    backend_hosts: Vec<String>,
    backend_port: u16,
    cert_path: PathBuf,
    key_path: PathBuf,
    lb_client_cert_path: PathBuf,
    lb_client_key_path: PathBuf,
    frontend_dir: PathBuf,
}

impl Config {
    fn from_env() -> Self {
        let public_port: u16 = env::var("LB_HTTPS_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8443);
        let public_http_port: u16 = env::var("LB_HTTP_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8080);
        let health_ingest_port: u16 = env::var("LB_HEALTH_INGEST_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(9443);
        let backend_hosts: Vec<String> = env::var("BACKEND_HOSTS")
            .unwrap_or_else(|_| "server1,server2,server3,server4,server5".into())
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let backend_port: u16 = env::var("BACKEND_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4443);
        let cert_path =
            PathBuf::from(env::var("CERT_PATH").unwrap_or_else(|_| "/app/certs/cert.pem".into()));
        let key_path =
            PathBuf::from(env::var("KEY_PATH").unwrap_or_else(|_| "/app/certs/key.pem".into()));
        let lb_client_cert_path = PathBuf::from(
            env::var("LB_CLIENT_CERT_PATH").unwrap_or_else(|_| "/app/certs/lb-client.pem".into()),
        );
        let lb_client_key_path = PathBuf::from(
            env::var("LB_CLIENT_KEY_PATH").unwrap_or_else(|_| "/app/certs/lb-client.key".into()),
        );
        let frontend_dir =
            PathBuf::from(env::var("FRONTEND_DIR").unwrap_or_else(|_| "/app/frontend".into()));

        Self {
            public_port,
            public_http_port,
            health_ingest_port,
            backend_hosts,
            backend_port,
            cert_path,
            key_path,
            lb_client_cert_path,
            lb_client_key_path,
            frontend_dir,
        }
    }
}

fn run_health_listener(port: u16, tls_server: Arc<rustls::ServerConfig>, registry: Arc<Registry>) {
    let listener = TcpListener::bind(("0.0.0.0", port)).expect("bind health listener");
    let ctx = Arc::new(HealthCtx { registry });
    for incoming in listener.incoming() {
        let mut sock = match incoming {
            Ok(s) => s,
            Err(e) => {
                eprintln!("health accept error: {}", e);
                continue;
            }
        };
        let _ = sock.set_read_timeout(Some(Duration::from_secs(10)));
        let _ = sock.set_write_timeout(Some(Duration::from_secs(10)));
        let ctx = ctx.clone();
        let tls_server = tls_server.clone();
        thread::spawn(move || {
            let mut tls = match ServerConnection::new(tls_server) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("health tls setup failed: {}", e);
                    return;
                }
            };
            handle_health_connection(&ctx, &mut tls, &mut sock);
        });
    }
}

fn handle_health_connection(
    ctx: &Arc<HealthCtx>,
    tls: &mut ServerConnection,
    sock: &mut TcpStream,
) {
    let mut stream = Stream::new(tls, sock);
    let mut reader = BufReader::new(&mut stream);
    let req = match read_request(&mut reader) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("health: failed to parse request: {}", e);
            return;
        }
    };
    drop(reader);
    let resp = health::handle(ctx, &req);
    if let Err(e) = write_response(&mut stream, &resp) {
        eprintln!("health: failed to write response: {}", e);
    }
    tls.send_close_notify();
}

fn run_public_listener(
    port: u16,
    tls_server: Arc<rustls::ServerConfig>,
    router_ctx: Arc<RouterCtx>,
) {
    let listener = TcpListener::bind(("0.0.0.0", port)).expect("bind public listener");
    for incoming in listener.incoming() {
        let mut sock = match incoming {
            Ok(s) => s,
            Err(e) => {
                eprintln!("public accept error: {}", e);
                continue;
            }
        };
        let client_ip: IpAddr = sock
            .peer_addr()
            .map(|a| a.ip())
            .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
        let _ = sock.set_read_timeout(Some(Duration::from_secs(60)));
        let _ = sock.set_write_timeout(Some(Duration::from_secs(60)));

        let router_ctx = router_ctx.clone();
        let tls_server = tls_server.clone();
        thread::spawn(move || {
            let mut tls = match ServerConnection::new(tls_server) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("public tls setup failed: {}", e);
                    return;
                }
            };
            handle_public_connection(&router_ctx, &mut tls, &mut sock, client_ip);
        });
    }
}

fn handle_public_connection(
    router_ctx: &Arc<RouterCtx>,
    tls: &mut ServerConnection,
    sock: &mut TcpStream,
    client_ip: IpAddr,
) {
    let mut stream = Stream::new(tls, sock);
    let mut reader = BufReader::new(&mut stream);
    let req = match read_request(&mut reader) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("public: failed to parse request: {}", e);
            return;
        }
    };
    drop(reader);
    let resp = router::route(router_ctx, client_ip, &req);
    if let Err(e) = write_response(&mut stream, &resp) {
        eprintln!("public: failed to write response: {}", e);
    }
    tls.send_close_notify();
}

fn run_public_http_listener(port: u16, router_ctx: Arc<RouterCtx>) {
    let listener = TcpListener::bind(("0.0.0.0", port)).expect("bind public http listener");
    for incoming in listener.incoming() {
        let sock = match incoming {
            Ok(s) => s,
            Err(e) => {
                eprintln!("public http accept error: {}", e);
                continue;
            }
        };
        let client_ip: IpAddr = sock
            .peer_addr()
            .map(|a| a.ip())
            .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
        let _ = sock.set_read_timeout(Some(Duration::from_secs(60)));
        let _ = sock.set_write_timeout(Some(Duration::from_secs(60)));

        let router_ctx = router_ctx.clone();
        thread::spawn(move || handle_public_plain_connection(&router_ctx, sock, client_ip));
    }
}

fn handle_public_plain_connection(
    router_ctx: &Arc<RouterCtx>,
    mut sock: TcpStream,
    client_ip: IpAddr,
) {
    let mut reader = BufReader::new(&mut sock);
    let req = match read_request(&mut reader) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("public http: failed to parse request: {}", e);
            return;
        }
    };
    drop(reader);
    let resp = router::route(router_ctx, client_ip, &req);
    if let Err(e) = write_response(&mut sock, &resp) {
        eprintln!("public http: failed to write response: {}", e);
    }
}
