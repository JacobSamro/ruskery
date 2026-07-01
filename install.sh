#!/bin/sh
# ruskery one-line installer.
#
#   curl -fsSL https://raw.githubusercontent.com/jacobsamro/ruskery/master/install.sh | sh
#
# Downloads the matching binary (verified against SHA256SUMS), installs a
# systemd service, and writes a starter config. Re-running upgrades in place.
#
# Environment:
#   RUSKERY_VERSION   pin a release (default: latest)
#   RUSKERY_REPO      github owner/repo (default: jacobsamro/ruskery)
#   RUSKERY_PREFIX    install prefix for the binary (default: /usr/local/bin)
set -eu

REPO="${RUSKERY_REPO:-jacobsamro/ruskery}"
VERSION="${RUSKERY_VERSION:-latest}"
PREFIX="${RUSKERY_PREFIX:-/usr/local/bin}"
CONFIG_DIR="/etc/ruskery"
DATA_DIR="/var/lib/ruskery"
SERVICE="/etc/systemd/system/ruskery.service"

log()  { printf '\033[1;33m==>\033[0m %s\n' "$*"; }
err()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

[ "$(id -u)" -eq 0 ] || err "please run as root (e.g. pipe into 'sudo sh')"

# ── detect platform ──────────────────────────────────────────────
os="$(uname -s | tr '[:upper:]' '[:lower:]')"
[ "$os" = "linux" ] || err "ruskery's installer targets Linux (got: $os)"
case "$(uname -m)" in
  x86_64|amd64)   arch="x86_64" ;;
  aarch64|arm64)  arch="aarch64" ;;
  *) err "unsupported architecture: $(uname -m)" ;;
esac
asset="ruskery-${arch}-unknown-linux-gnu"

for tool in curl tar sha256sum install; do
  command -v "$tool" >/dev/null 2>&1 || err "missing required tool: $tool"
done

# ── resolve version ──────────────────────────────────────────────
if [ "$VERSION" = "latest" ]; then
  log "resolving latest release of $REPO"
  # Buffer the response before grepping: piping curl straight into `grep -m1`
  # lets grep close the pipe on the first match, and curl then reports
  # "(23) Failure writing output to destination" on the broken pipe.
  release_json="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest")"
  VERSION="$(printf '%s\n' "$release_json" | grep -m1 '"tag_name"' | cut -d'"' -f4)"
  [ -n "$VERSION" ] || err "could not determine latest version"
fi

# Release tags carry a leading "v" (v0.8.1); the binary reports a bare semver
# (0.8.1). Compare and display the bare form so re-running an up-to-date install
# isn't mistaken for an upgrade. The download URL below still uses $VERSION (tag).
VERSION_NUM="${VERSION#v}"

# Detect an existing install so we can report (and behave like) an upgrade.
OLD_VERSION=""
if [ -x "$PREFIX/ruskery" ]; then
  OLD_VERSION="$("$PREFIX/ruskery" --version 2>/dev/null | awk '{print $2}')"
fi
if [ -n "$OLD_VERSION" ] && [ "$OLD_VERSION" != "$VERSION_NUM" ]; then
  log "upgrading ruskery $OLD_VERSION -> $VERSION_NUM ($arch)"
elif [ -n "$OLD_VERSION" ]; then
  log "reinstalling ruskery $VERSION_NUM ($arch)"
else
  log "installing ruskery $VERSION_NUM ($arch)"
fi

base="https://github.com/${REPO}/releases/download/${VERSION}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# ── download + verify ────────────────────────────────────────────
log "downloading $asset.tar.gz"
curl -fsSL "$base/${asset}.tar.gz" -o "$tmp/ruskery.tar.gz"
curl -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS"

log "verifying checksum"
want="$(grep " ${asset}.tar.gz\$" "$tmp/SHA256SUMS" | awk '{print $1}')"
[ -n "$want" ] || err "checksum for ${asset}.tar.gz not found in SHA256SUMS"
got="$(sha256sum "$tmp/ruskery.tar.gz" | awk '{print $1}')"
[ "$want" = "$got" ] || err "checksum mismatch (expected $want, got $got)"

tar -xzf "$tmp/ruskery.tar.gz" -C "$tmp"

# ── install binary ───────────────────────────────────────────────
log "installing binary to $PREFIX/ruskery"
install -m 0755 "$tmp/ruskery" "$PREFIX/ruskery"

# ── user, dirs, config ───────────────────────────────────────────
if ! id ruskery >/dev/null 2>&1; then
  log "creating system user 'ruskery'"
  useradd --system --home "$DATA_DIR" --shell /usr/sbin/nologin ruskery 2>/dev/null \
    || adduser --system --home "$DATA_DIR" --no-create-home ruskery 2>/dev/null || true
fi
mkdir -p "$CONFIG_DIR" "$DATA_DIR"
chown -R ruskery:ruskery "$DATA_DIR" 2>/dev/null || chown -R ruskery "$DATA_DIR" 2>/dev/null || true

if [ ! -f "$CONFIG_DIR/config.toml" ]; then
  log "writing starter config to $CONFIG_DIR/config.toml"
  cat > "$CONFIG_DIR/config.toml" <<'TOML'
[server]
http_addr = "0.0.0.0:80"
https_addr = "0.0.0.0:443"
# public_url = "https://registry.example.com"

[database]
path = "/var/lib/ruskery/ruskery.db"

[storage]
endpoint = "https://t3.storage.dev"
bucket = ""                # <-- your Tigris bucket
region = "auto"
# Prefer setting these via environment in the systemd unit / drop-in:
#   RUSKERY_STORAGE__ACCESS_KEY_ID, RUSKERY_STORAGE__SECRET_ACCESS_KEY
access_key_id = ""
secret_access_key = ""

[tls]
# Automatic Let's Encrypt is on. The server stays on plain HTTP until you add a
# domain in the dashboard, then provisions a cert on :443 automatically.
enabled = true
contact_email = ""         # recommended: your email, for Let's Encrypt notices
TOML
  chmod 640 "$CONFIG_DIR/config.toml"
  chown root:ruskery "$CONFIG_DIR/config.toml" 2>/dev/null || true
fi

# ── database migrations ──────────────────────────────────────────
# `serve` also migrates on start, but run it now (as the ruskery user, so the
# DB file keeps the right owner) so an upgrade's schema changes land before the
# service comes back up. Idempotent and safe to repeat.
if id ruskery >/dev/null 2>&1 && command -v su >/dev/null 2>&1; then
  log "applying database migrations"
  # Pass paths as positional args ($1, $2) rather than interpolating them into
  # the shell command, so a crafted RUSKERY_PREFIX can't inject shell syntax.
  su -s /bin/sh ruskery -c 'exec "$1" --config "$2" migrate' ruskery-migrate \
    "$PREFIX/ruskery" "$CONFIG_DIR/config.toml" \
    >/dev/null 2>&1 || log "migrations will run on next service start"
fi

# ── systemd service ──────────────────────────────────────────────
if command -v systemctl >/dev/null 2>&1; then
  log "installing systemd service"
  curl -fsSL "$base/ruskery.service" -o "$SERVICE" 2>/dev/null || cat > "$SERVICE" <<'UNIT'
[Unit]
Description=ruskery container registry
After=network-online.target
Wants=network-online.target

[Service]
Type=exec
User=ruskery
Group=ruskery
ExecStart=/usr/local/bin/ruskery --config /etc/ruskery/config.toml serve
Restart=on-failure
RestartSec=2s
AmbientCapabilities=CAP_NET_BIND_SERVICE
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/ruskery
StateDirectory=ruskery
LimitNOFILE=1048576

[Install]
WantedBy=multi-user.target
UNIT
  systemctl daemon-reload
  systemctl enable ruskery >/dev/null 2>&1 || true
  log "starting ruskery"
  systemctl restart ruskery || log "could not start yet — finish editing $CONFIG_DIR/config.toml, then: systemctl restart ruskery"
fi

if [ -n "$OLD_VERSION" ] && [ "$OLD_VERSION" != "$VERSION_NUM" ]; then
  cat <<EOF

ruskery upgraded: $OLD_VERSION -> $VERSION_NUM

  Binary : $PREFIX/ruskery   (replaced)
  Config : $CONFIG_DIR/config.toml   (kept)
  Data   : $DATA_DIR   (kept; migrations applied)

The service was restarted on the new version. Logs: journalctl -u ruskery -f
EOF
elif [ -n "$OLD_VERSION" ]; then
  cat <<EOF

ruskery is already at $VERSION_NUM — reinstalled and restarted.

Logs: journalctl -u ruskery -f
EOF
else
  cat <<EOF

ruskery $VERSION_NUM installed.

  Binary : $PREFIX/ruskery
  Config : $CONFIG_DIR/config.toml   (set your Tigris bucket + keys)
  Data   : $DATA_DIR

Next steps:
  1. Edit $CONFIG_DIR/config.toml (Tigris bucket + credentials).
  2. systemctl restart ruskery
  3. Open http://<server> and complete the setup wizard.
  4. Add your domain in Settings → Domains & TLS to enable HTTPS.

Logs: journalctl -u ruskery -f
EOF
fi
