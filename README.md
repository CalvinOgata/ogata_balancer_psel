# ogata_balancer

A load balancer built from scratch in Rust, for learning how TCP connections, HTTP parsing, and request routing work at the implementation level. No async runtime, no HTTP framework, no Nginx — just `TcpListener`, `TcpStream`, and raw byte parsing.

---

## What it does

The system distributes incoming file requests across five backend servers. Each backend holds a set of PDF files and serves one at random per request. The load balancer tracks how busy each server is and routes new requests to the least-loaded one, keeping the same client pinned to the same server for the duration of a session.

A live dashboard in the browser shows all five servers, their current load, and which one served the last request.

---

## Architecture

```
Browser
   │  HTTP :8080  /  HTTPS :8443
   ▼
┌──────────────────────────────────────┐
│            load_balancer             │
│                                      │
│  • serves the frontend (HTMX)        │
│  • routes /file to a backend         │
│  • receives health reports           │
│  • maintains sticky sessions         │
└────────────┬─────────────────────────┘
             │ mTLS :4443
   ┌─────────┼─────────┐
   ▼         ▼         ▼  ...
server1   server2   server3-5
```

Three Rust crates in a workspace, six Docker containers on a shared private network (`balancer_net`).

**`shared`** — code used by both sides: a hand-written HTTP/1.1 parser, TLS configuration builders, and the `HealthReport` type that servers use to report their status.

**`load_balancer`** — four modules:
- `health` — the backend registry, sticky-session table, and the health-ingest endpoint that servers POST to
- `scheduler` — picks the lowest-load live backend
- `proxy` — opens a fresh mTLS connection to the chosen backend, forwards the request, sanitizes the response
- `router` — dispatches requests: static files served from disk, `/api/servers` renders the server table, everything else proxied

**`server`** — two threads per container:
- Main thread accepts mTLS connections and serves random PDFs
- `health-reporter` thread pushes a load snapshot to the LB every 2 seconds

---

## Load balancing

Servers continuously self-report their load as `in_flight / 32` — the ratio of concurrent requests being handled to the server's notional capacity. The LB always routes to the live backend with the lowest ratio.

New clients are pinned to their chosen backend via a sticky-session table keyed by client IP (one-hour TTL). If the pinned backend goes stale, the client is re-routed on the next request.

A backend is considered stale if no health report has arrived in the last 10 seconds.

---

## The backend servers

All five backends run the same binary, distinguished only by the `SERVER_ID` environment variable. PDFs are copied into the Docker image at build time — each container is self-contained.

Each server runs two threads:

**Main thread** — binds to port 4443, checks every incoming IP against the allowlist, performs the mTLS handshake, then handles the request. It picks a random PDF from `/app/files/`, reads it, and returns it with `Content-Type: application/pdf` and `X-Server-Id` so the dashboard knows which backend responded.

Load is tracked with an atomic `in_flight` counter and an RAII guard (`InFlightGuard`) that increments on connection accept and decrements when the handler returns — including on error paths.

**`health-reporter` thread** — every 2 seconds, reads the current `in_flight` count and `available` flag, and POSTs a `HealthReport` to the LB's health-ingest port. If a report fails, it logs the error and retries at the next interval. If the LB stops hearing from a server for 10 seconds, it marks that backend stale and stops routing to it.

---

## HTTP parsing (`protocol.rs`)

There is no HTTP library in this project. Every request and response is parsed by hand in `shared/src/protocol.rs` using only `std::io::BufReader`.

Parsing happens in three stages: the request line (method, target, version), then headers (split into `Vec<(String, String)>`), then the body — but only if `Content-Length` is present, and capped at 1 MB to prevent memory exhaustion.

If both `Content-Length` and `Transfer-Encoding` appear in the same request, it is rejected outright. Accepting both simultaneously is the classic request-smuggling setup, where two parties disagree on where one request ends and the next begins.

`HealthReport` uses a simple `key=value\n` text encoding rather than JSON or HTTP headers, keeping the health channel entirely separate from the request parsing path.

---

## Security

**Docker bridge network** — backends bind on port 4443 but that port is never published to the host. Nothing outside the Docker network has a route to them.

**IP allowlist** — the first check after `accept()`, before TLS, before reading any bytes. Each backend resolves the LB's hostname at startup and drops any TCP connection from an unexpected IP.

**Mutual TLS (TLS 1.3 only)** — both sides authenticate. When the LB connects to a backend, it presents a client certificate (`lb-client-chain.pem`). The backend verifies it against a dedicated CA (`lb-ca.pem`). The CA's private key is never mounted in any container — only the cert is — so a compromised backend cannot forge LB credentials.

TLS 1.2 is disabled at the dependency level (`rustls` compiled without the `tls12` feature).

---

## Running it

**Prerequisites:** Docker, Docker Compose, `openssl`.

**1. Generate certificates**

```bash
# Shared server cert (LB public port + all backends)
openssl req -x509 -newkey rsa:4096 -keyout certs/key.pem -out certs/cert.pem \
  -days 365 -nodes -subj "/CN=localhost" \
  -addext "subjectAltName=DNS:localhost,DNS:server1,DNS:server2,DNS:server3,DNS:server4,DNS:server5"

# LB client CA
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out certs/lb-ca.key
openssl req -x509 -new -key certs/lb-ca.key -out certs/lb-ca.pem \
  -days 365 -subj "/CN=lb-ca" \
  -addext "basicConstraints=critical,CA:TRUE,pathlen:0" \
  -addext "keyUsage=critical,keyCertSign,cRLSign"

# LB client cert signed by the CA
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out certs/lb-client.key
openssl req -new -key certs/lb-client.key -out certs/lb-client.csr -subj "/CN=lb-client"
openssl x509 -req -in certs/lb-client.csr -CA certs/lb-ca.pem -CAkey certs/lb-ca.key \
  -CAcreateserial -out certs/lb-client.pem -days 365 \
  -extfile <(echo "basicConstraints=CA:FALSE\nkeyUsage=critical,digitalSignature")
cat certs/lb-client.pem certs/lb-ca.pem > certs/lb-client-chain.pem
```

**2. Start**

```bash
docker compose up --build
```

**3. Open**

```
http://localhost:8080
```

---

## Project layout

```
ogata_balancer/
├── load_balancer/
│   └── src/
│       ├── main.rs        # two TLS listeners + accept loops
│       ├── health.rs      # registry, sticky sessions, health ingest
│       ├── proxy.rs       # mTLS forward to backend
│       ├── router.rs      # request dispatch
│       └── scheduler.rs   # lowest-load picker
├── server/
│   └── src/
│       ├── main.rs        # accept loop + health reporter thread
│       ├── handler.rs     # request handler + access log
│       └── health.rs      # in-flight counter, HealthState
├── shared/
│   └── src/
│       ├── protocol.rs    # HTTP/1.1 parser + HealthReport
│       └── tls.rs         # rustls config builders
├── frontend/
│   └── index.html         # HTMX dashboard
├── certs/                 # TLS certificates (not committed)
└── docker-compose.yml
```
