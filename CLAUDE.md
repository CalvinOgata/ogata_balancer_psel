# Load Balancer - Development Guide

This file provides guidance to Claude Code when working with code in this repository.

## Project Overview

`ogata_balancer` is a Rust project (edition 2024) for learning how load balancing and TCP/HTTP requests work in real scenarios. It uses Rust as the main programming language, with the `TcpListener` and `TcpStream` standard library types to handle requests, Docker instances to run multiple servers on the same machine, and HTMX on the frontend for a clean user interface. The servers serve a random PDF file from the `files` folder whenever the load balancer forwards a request.

## Key Architecture Decisions

- **Backend and load balancer: Rust only.** Since this is primarily a learning project, Rust must be used for every line of code concerning the main logic and processes of the file distribution system. No Python, JavaScript, or any other language. No Tokio, HTTP parsers, or similar libraries — only raw `TcpListener` and `TcpStream`. Everything must be handled manually, including HTTP parsing.
- **Frontend: HTMX only.** The same constraint applies to the frontend, with HTMX replacing Rust. The frontend must be pure HTMX — no JS frameworks, no Tailwind, nothing else.
- **Servers: Docker Compose containers with raw integration.** As a learning project, the servers run as Docker Compose containers but must not use any shortcuts (such as Nginx) for request handling or parsing. Each server must have its own independent code for handling requests. There must be exactly 5 servers.
- **Security-first approach.** In addition to servers rejecting every request that does not originate from the load balancer, and there must be mTLS encryption so that only the entity holding the client-key can complete a handshake with the servers.
- **Health and performance.** The load balancing algorithm must use a resource-based approach: servers continuously monitor themselves and report their health status to the load balancer. For example, if Server 1 is at 50% capacity and Server 2 is at 25%, the load balancer must route the client to Server 2, and all subsequent requests from that client must be handled by that server. If a server is being updated or shut down, it signals to the load balancer that it is unavailable, and another server must be chosen.

## File Structure

```
ogata_balancer/
│
├── Cargo.toml
├── Cargo.lock
├── docker-compose.yml          # Defines all 5 server containers + load balancer
├── .env                        # APP_SECRET_TOKEN, ports, etc.
├── README.md
│
├── load_balancer/
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs
│       ├── proxy.rs
│       ├── router.rs
│       ├── scheduler.rs
│       └── health.rs
│
├── server/
│   ├── Cargo.toml
│   ├── Dockerfile              # Builds the server image once
│   └── src/
│       ├── main.rs
│       ├── handler.rs
│       ├── health.rs
│       └── files/              # PDFs copied into the image at build time
│           ├── doc1.pdf
│           ├── doc2.pdf
│           └── ...
│
├── shared/
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── token.rs
│       ├── protocol.rs
│       └── tls.rs
│
└── frontend/
    ├── index.html
    └── assets/
        └── style.css
```

## Commands

```bash
# Build
cargo build

# Build release
cargo build --release

# Run
cargo run

# Check (faster than build, no binary output)
cargo check

# Run tests
cargo test

# Run a single test by name
cargo test <test_name>

# Lint
cargo clippy

# Format
cargo fmt
```
