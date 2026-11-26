#!/usr/bin/env bash
set -euo pipefail

# Standalone macOS & Linux installer for kern.
# Usage:
#   ./scripts/install.sh                    # build & install kern into ~/.local/bin
#   ./scripts/install.sh --bin-dir /usr/local/bin
#   ./scripts/install.sh --setup            # also register kern MCP & compaction hooks

BIN_DIR="${KERN_BIN_DIR:-$HOME/.local/bin}"
RUN_SETUP=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin-dir) BIN_DIR="${2:?--bin-dir requires a directory}"; shift 2 ;;
    --setup) RUN_SETUP=1; shift ;;
    -h|--help)
      cat <<'EOF'
Usage: ./scripts/install.sh [OPTIONS]
  --bin-dir <dir>   Target directory for the kern binary (default: ~/.local/bin)
  --setup           Run `kern setup` after install to register MCP & compaction hooks
EOF
      exit 0
      ;;
    *) echo "Unknown option: $1" >&2; exit 1 ;;
  esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "==> Building kern (release)..."
cargo build --release --manifest-path "$REPO_DIR/Cargo.toml"

mkdir -p "$BIN_DIR"
cp "$REPO_DIR/target/release/kern" "$BIN_DIR/kern.new"
chmod 755 "$BIN_DIR/kern.new"
mv -f "$BIN_DIR/kern.new" "$BIN_DIR/kern"

echo "==> Installed $BIN_DIR/kern ($("$BIN_DIR/kern" --version))"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *)
    echo "Note: $BIN_DIR is not on your PATH. Add this to your shell rc file:"
    echo "  export PATH=\"$BIN_DIR:\$PATH\""
    ;;
esac

if [[ "$RUN_SETUP" -eq 1 ]]; then
  "$BIN_DIR/kern" setup
fi
