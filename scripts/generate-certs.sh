#!/usr/bin/env bash
# Generate a self-signed CA + leaf cert covering the load balancer and all
# backend servers. Mounts into both containers via docker-compose.
#
# This is a learning project — we use a single shared cert for simplicity. Run
# once after cloning, then again whenever the cert expires.

set -euo pipefail

cd "$(dirname "$0")/../certs"

if [[ -f cert.pem && -f key.pem ]]; then
  echo "certs already exist; delete cert.pem and key.pem to regenerate"
  exit 0
fi

CONFIG=$(mktemp)
cat >"$CONFIG" <<'EOF'
[req]
default_bits       = 2048
prompt             = no
default_md         = sha256
distinguished_name = dn
req_extensions     = v3_req

[dn]
CN = ogata_balancer

[v3_req]
keyUsage          = digitalSignature, keyEncipherment
extendedKeyUsage  = serverAuth, clientAuth
subjectAltName    = @alt_names

[alt_names]
DNS.1 = localhost
DNS.2 = load_balancer
DNS.3 = server1
DNS.4 = server2
DNS.5 = server3
DNS.6 = server4
DNS.7 = server5
IP.1  = 127.0.0.1
EOF

openssl req -x509 -newkey rsa:2048 -keyout key.pem -out cert.pem \
  -days 365 -nodes -config "$CONFIG" -extensions v3_req

rm -f "$CONFIG"

chmod 644 cert.pem key.pem
echo "Wrote certs/cert.pem and certs/key.pem"
