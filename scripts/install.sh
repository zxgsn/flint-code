#!/usr/bin/env bash
# flint — Linux/macOS build & install script
# Usage:
#   bash scripts/install.sh              # Build + install to ~/.local/bin
#   bash scripts/install.sh --run        # Build + install + launch config TUI
#   bash scripts/install.sh --dir /opt/bin  # Install to custom directory

set -euo pipefail

DIR="$HOME/.local/bin"
RUN=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --run) RUN=true; shift ;;
        --dir) DIR="$2"; shift 2 ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

echo "=== flint installer ==="
echo ""

# ── 1. Build ──────────────────────────────────────────────────────────────
echo -e "\033[33m[1/3] Building release...\033[0m"
cd "$PROJECT_ROOT"
cargo build --release 2>&1 | sed 's/^/  /'

BINARY="$PROJECT_ROOT/target/release/flint"
if [[ ! -f "$BINARY" ]]; then
    echo -e "\033[31mBinary not found at $BINARY\033[0m"
    exit 1
fi
echo -e "  \033[32mOK: $BINARY\033[0m"

# ── 2. Install ────────────────────────────────────────────────────────────
echo -e "\033[33m[2/3] Installing to $DIR ...\033[0m"
mkdir -p "$DIR"
cp "$BINARY" "$DIR/flint"
chmod +x "$DIR/flint"
echo -e "  \033[32mOK: $DIR/flint\033[0m"

# Check PATH
if [[ ":$PATH:" != *":$DIR:"* ]]; then
    echo ""
    echo -e "  \033[33mNOTE: $DIR is not in your PATH.\033[0m"
    echo -e "  \033[33mAdd it to your shell profile:\033[0m"
    echo -e "    \033[37mecho 'export PATH=\"\$PATH:$DIR\"' >> ~/.bashrc\033[0m"
    echo ""
fi

# ── 3. Done ───────────────────────────────────────────────────────────────
echo -e "\033[32m[3/3] Done!\033[0m"
echo ""
echo -e "  \033[90mBinary : $DIR/flint\033[0m"
echo -e "  \033[90mUsage  : flint config\033[0m"
echo -e "           \033[90mflint 'your prompt here'\033[0m"
echo ""

if $RUN; then
    echo -e "\033[36mLaunching flint config...\033[0m"
    "$DIR/flint" config
fi
