#!/usr/bin/env bash
set -euo pipefail

echo "=== Mycena Node - Raspberry Pi Setup ==="

sudo useradd -r -s /bin/false mycena 2>/dev/null || true
sudo mkdir -p /var/lib/mycena
sudo chown mycena:mycena /var/lib/mycena

if [ ! -f /usr/local/bin/mycena ]; then
  echo "ERROR: /usr/local/bin/mycena not found. Copy the binary first:"
  echo "  scp target/aarch64-unknown-linux-gnu/release/mycena pi@<IP>:/usr/local/bin/"
  exit 1
fi

sudo chmod +x /usr/local/bin/mycena
sudo mkdir -p /etc/mycena
cat << "EOF" | sudo tee /etc/mycena/node.env >/dev/null
RUST_LOG=info
MYCENA_LISTEN=/ip4/0.0.0.0/tcp/7761
MYCENA_DB=/var/lib/mycena
MYCENA_NAME=pi-node
MYCENA_API_PORT=7760
EOF

cat << "EOF" | sudo tee /etc/systemd/system/mycena.service >/dev/null
[Unit]
Description=Mycena Mesh Node
After=network.target
Wants=network-online.target

[Service]
Type=simple
User=mycena
EnvironmentFile=/etc/mycena/node.env
ExecStart=/usr/local/bin/mycena \
  --listen ${MYCENA_LISTEN} \
  --db ${MYCENA_DB} \
  --name ${MYCENA_NAME} \
  --api-port ${MYCENA_API_PORT}
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable mycena
sudo systemctl start mycena

echo "=== Setup complete ==="
echo "Status: sudo systemctl status mycena"
echo "Logs:   sudo journalctl -u mycena -f"
echo "API:    http://localhost:7760/api/v1/status"
