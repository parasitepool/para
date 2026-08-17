#!/bin/bash

# Installs ckproxy from the latest git source and sets it up as a systemd
# service, prompting for upstream pool(s) and the local port to bind to.

# Exit on errors
set -e

GIT_URL="https://bitbucket.org/ckolivas/ckpool.git"
SRC_DIR="/opt/ckpool"
CONF_DIR="/etc/ckpool"
CONF_FILE="$CONF_DIR/ckproxy.conf"
LOG_DIR="/var/log/ckproxy"
SERVICE_FILE="/etc/systemd/system/ckproxy.service"

# Function to detect distro and set package manager
detect_distro() {
    if [ -f /etc/os-release ]; then
        . /etc/os-release
        DISTRO=$ID
    else
        echo "Unsupported distribution. Exiting."
        exit 1
    fi
    case $DISTRO in
        ubuntu|debian)
            INSTALL_CMD="apt install -y"
            UPDATE_CMD="apt update"
            PACKAGES="build-essential git autoconf automake libtool pkg-config yasm libzmq3-dev"
            ;;
        fedora|centos|rhel)
            INSTALL_CMD="dnf install -y"
            UPDATE_CMD="dnf check-update || true"
            PACKAGES="gcc gcc-c++ make git autoconf automake libtool pkgconf-pkg-config yasm zeromq-devel"
            ;;
        *)
            echo "Unsupported distribution: $DISTRO. Exiting."
            exit 1
            ;;
    esac
}

# Parse a pool url into PARSED_HOST / PARSED_PORT / PARSED_SV2.
# Accepts host:port, stratum+tcp://host:port, host:port/KEY and
# stratum2+tcp://host:port/KEY forms. A path component means SV2.
parse_pool_url() {
    local url="$1" rest hostport
    rest="${url#*://}"
    if [ "${rest%%/*}" != "$rest" ]; then
        PARSED_SV2=true
        hostport="${rest%%/*}"
    else
        PARSED_SV2=false
        hostport="$rest"
    fi
    PARSED_HOST="${hostport%:*}"
    PARSED_PORT="${hostport##*:}"
    if [ -z "$PARSED_HOST" ] || [ "$PARSED_HOST" = "$PARSED_PORT" ] || \
       ! [[ "$PARSED_PORT" =~ ^[0-9]+$ ]] || [ "$PARSED_PORT" -lt 1 ] || [ "$PARSED_PORT" -gt 65535 ]; then
        return 1
    fi
    return 0
}

# Test an upstream pool connection. SV1 pools get a full stratum
# mining.subscribe handshake; SV2 pools are Noise-encrypted so only the TCP
# connection is tested.
test_pool() {
    local host="$1" port="$2" sv2="$3" req resp
    if [ "$sv2" = true ]; then
        echo "Testing TCP connection to $host:$port (SV2 - handshake is encrypted, testing reachability only)..."
        if timeout 10 bash -c 'exec 3<>"/dev/tcp/$0/$1"' "$host" "$port" 2>/dev/null; then
            echo "Connection to $host:$port succeeded."
            return 0
        fi
        echo "Could not connect to $host:$port."
        return 1
    fi
    echo "Testing stratum connection to $host:$port..."
    req='{"id": 1, "method": "mining.subscribe", "params": ["ckproxy-installer"]}'
    resp=$(timeout 10 bash -c 'exec 3<>"/dev/tcp/$0/$1"; printf "%s\n" "$2" >&3; head -n 1 <&3' \
        "$host" "$port" "$req" 2>/dev/null || true)
    if [[ "$resp" == *'"result"'* ]]; then
        echo "Connection to $host:$port succeeded (valid stratum response received)."
        return 0
    fi
    if [ -n "$resp" ]; then
        echo "Connected to $host:$port but did not receive a valid stratum response."
    else
        echo "Could not connect to $host:$port."
    fi
    return 1
}

# Escape a string for embedding in JSON
json_escape() {
    printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
}

# Check if sudo
if [ "$EUID" -ne 0 ]; then
    echo "Please run with sudo or as root."
    exit 1
fi

# Detect previous installation
if [ -f "$SERVICE_FILE" ] || [ -f "$CONF_FILE" ]; then
    read -p "Previous ckproxy installation detected. Overwrite existing config and service? (y/N, default: no): " overwrite_answer
    if [[ ! "$overwrite_answer" =~ ^[Yy]$ ]]; then
        echo "Installation aborted."
        exit 0
    fi
    echo "Overwriting previous installation..."
    systemctl stop ckproxy 2>/dev/null || true
    systemctl disable ckproxy 2>/dev/null || true
fi

echo "Starting installation of ckproxy. This requires sudo privileges."

# Prompt for service user (default to current sudo user)
current_user=${SUDO_USER:-root}
echo "Optionally, choose a user to run ckproxy as (instead of $current_user)."
read -p "Enter existing username, or 'create' to make a new 'ckpool' user (leave blank for $current_user): " input_user
if [ "$input_user" = "create" ]; then
    id ckpool >/dev/null 2>&1 || useradd -m -s /bin/bash ckpool
    service_user="ckpool"
elif [ -z "$input_user" ]; then
    service_user="$current_user"
else
    if id "$input_user" >/dev/null 2>&1; then
        service_user="$input_user"
    else
        echo "User $input_user does not exist. Exiting."
        exit 1
    fi
fi

detect_distro
echo "Installing dependencies..."
eval $UPDATE_CMD
$INSTALL_CMD $PACKAGES

# Fetch latest ckpool source
if [ -d "$SRC_DIR/.git" ]; then
    echo "Updating existing ckpool source in $SRC_DIR..."
    git -C "$SRC_DIR" pull
else
    echo "Cloning ckpool into $SRC_DIR..."
    git clone "$GIT_URL" "$SRC_DIR"
fi

# Build and install
cd "$SRC_DIR"
./autogen.sh
./configure
make -j$(nproc)
make install

# Prompt for upstream pools
echo
echo "Enter the upstream pool(s) for ckproxy to connect to. Pools are tried in"
echo "order with automatic failover. URL formats:"
echo "  host:port                     Stratum V1 (e.g. stratum.ckpool.org:3333)"
echo "  host:port/AUTHORITYKEY        Stratum V2 (e.g. stratum.ckpool.org:3336/9anrRNhBh7869XtNnFcCuGBRZP51E635qGbu457J5kHdszhfRc3)"
proxy_entries=""
pool_count=0
while true; do
    echo
    read -p "Upstream pool URL: " pool_url
    if [ -z "$pool_url" ]; then
        if [ $pool_count -eq 0 ]; then
            echo "At least one upstream pool is required."
            continue
        fi
        break
    fi
    if ! parse_pool_url "$pool_url"; then
        echo "Invalid pool URL '$pool_url'. Expected host:port (with optional scheme and SV2 key)."
        continue
    fi
    if ! test_pool "$PARSED_HOST" "$PARSED_PORT" "$PARSED_SV2"; then
        read -p "Connection test failed. Add this pool anyway? (y/N, default: no): " keep_answer
        if [[ ! "$keep_answer" =~ ^[Yy]$ ]]; then
            echo "Pool discarded."
            continue
        fi
    fi
    read -p "Username / BTC address for this pool: " pool_auth
    read -p "Password for this pool (often unused, default: x): " pool_pass
    if [ -z "$pool_pass" ]; then pool_pass="x"; fi
    if [ -n "$proxy_entries" ]; then
        proxy_entries+=$',\n'
    fi
    proxy_entries+=$'\t{\n'
    proxy_entries+=$'\t\t"url" : "'"$(json_escape "$pool_url")"$'",\n'
    proxy_entries+=$'\t\t"auth" : "'"$(json_escape "$pool_auth")"$'",\n'
    proxy_entries+=$'\t\t"pass" : "'"$(json_escape "$pool_pass")"$'"\n'
    proxy_entries+=$'\t}'
    pool_count=$((pool_count + 1))
    read -p "Add another (failover) pool? (y/N, default: no): " another_answer
    if [[ ! "$another_answer" =~ ^[Yy]$ ]]; then
        break
    fi
done

# Prompt for local bind port
echo
while true; do
    read -p "Local port for miners to connect to (default: 3334): " bind_port
    if [ -z "$bind_port" ]; then bind_port=3334; fi
    if ! [[ "$bind_port" =~ ^[0-9]+$ ]] || [ "$bind_port" -lt 1 ] || [ "$bind_port" -gt 65535 ]; then
        echo "Invalid port '$bind_port'."
        continue
    fi
    if ss -ltn 2>/dev/null | awk '{print $4}' | grep -q ":$bind_port\$"; then
        read -p "Port $bind_port appears to be in use. Use it anyway? (y/N, default: no): " port_answer
        if [[ ! "$port_answer" =~ ^[Yy]$ ]]; then
            continue
        fi
    fi
    break
done

# Write config
mkdir -p "$CONF_DIR" "$LOG_DIR"
cat << EOF > "$CONF_FILE"
{
"proxy" : [
$proxy_entries
],
"serverurl" : [
	"0.0.0.0:$bind_port"
],
"mindiff" : 1,
"startdiff" : 10000,
"logdir" : "$LOG_DIR"
}
EOF
chown -R $service_user:$service_user "$CONF_DIR" "$LOG_DIR"
echo "Wrote $CONF_FILE"

# Create systemd service
cat << EOF > "$SERVICE_FILE"
[Unit]
Description=CKPool Stratum Proxy
After=network-online.target
Wants=network-online.target

[Service]
User=$service_user
ExecStart=/usr/local/bin/ckproxy -q -c $CONF_FILE
StandardOutput=journal
StandardError=journal
Restart=always
RestartSec=5
LimitNOFILE=100000
LimitNPROC=65536

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable ckproxy
systemctl start ckproxy

echo
echo "Installation complete! ckproxy is running with $pool_count upstream pool(s)."
echo "Connect miners to: stratum+tcp://[machine IP]:$bind_port"
echo "Monitor logs:"
echo "  - journalctl -u ckproxy -f"
echo "  - tail -f $LOG_DIR/ckproxy.log"
echo "Edit $CONF_FILE if needed, then restart with: systemctl restart ckproxy"
