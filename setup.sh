#!/usr/bin/env bash
# =============================================================================
# Megadrive Studio — setup.sh
# Installation complète de l'environnement sur Ubuntu/Debian
# Usage : ./setup.sh [--no-vscode] [--no-blastem] [--no-clown] [--prefix /opt]
# =============================================================================
set -euo pipefail

# ── Options ──────────────────────────────────────────────────────────────────
PREFIX="${PREFIX:-$HOME/.local/megadrive-studio}"
INSTALL_VSCODE=true
INSTALL_BLASTEM=true
INSTALL_CLOWN=true
INSTALL_MARSDEV=true
INSTALL_MEGALINK=true

for arg in "$@"; do
  case $arg in
    --no-vscode)    INSTALL_VSCODE=false ;;
    --no-blastem)   INSTALL_BLASTEM=false ;;
    --no-clown)     INSTALL_CLOWN=false ;;
    --no-marsdev)   INSTALL_MARSDEV=false ;;
    --no-megalink)  INSTALL_MEGALINK=false ;;
    --prefix=*)     PREFIX="${arg#--prefix=}" ;;
  esac
done

BIN_DIR="$PREFIX/bin"
SRC_DIR="$PREFIX/src"
mkdir -p "$BIN_DIR" "$SRC_DIR"

BOLD='\033[1m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
log()  { echo -e "${GREEN}[setup]${NC} $*"; }
warn() { echo -e "${YELLOW}[warn]${NC}  $*"; }
fail() { echo -e "${RED}[fail]${NC}  $*"; exit 1; }
step() { echo -e "\n${BOLD}━━━ $* ━━━${NC}"; }

# ── Sanity checks ─────────────────────────────────────────────────────────────
[[ "$(uname -s)" == "Linux" ]] || fail "Ce script cible Linux (Ubuntu/Debian)."
command -v apt >/dev/null 2>&1 || fail "apt non trouvé — Ubuntu/Debian requis."

# ── System deps ───────────────────────────────────────────────────────────────
step "Dépendances système"
sudo apt-get update -qq
sudo apt-get install -y --no-install-recommends \
  build-essential git mercurial cmake ninja-build pkg-config curl wget \
  libsdl2-dev libsdl3-dev libglew-dev zlib1g-dev libpng-dev \
  libfreetype-dev \
  gdb-multiarch python3 python3-pip \
  openjdk-17-jre-headless \
  libusb-1.0-0-dev udev \
  jq bc

# ── Rust (pour megalink-rs) ────────────────────────────────────────────────────
if ! command -v cargo >/dev/null 2>&1; then
  step "Rust / Cargo"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
  source "$HOME/.cargo/env"
fi

# ── MARSDEV (m68k toolchain + SGDK) ───────────────────────────────────────────
if $INSTALL_MARSDEV; then
  step "MARSDEV — toolchain m68k-elf"
  MARSDEV_DIR="$SRC_DIR/marsdev"
  if [[ ! -d "$MARSDEV_DIR/.git" ]]; then
    git clone https://github.com/andwn/marsdev "$MARSDEV_DIR"
  else
    git -C "$MARSDEV_DIR" pull --ff-only
  fi
  cd "$MARSDEV_DIR"
  # Build seulement le toolchain m68k (pas SGDK — on le build séparément)
  make m68k-toolchain 2>&1 | tail -5
  # Symlinks dans BIN_DIR
  for f in "$MARSDEV_DIR"/m68k-elf/bin/m68k-elf-*; do
    ln -sf "$f" "$BIN_DIR/$(basename "$f")" 2>/dev/null || true
  done
  ln -sf "$MARSDEV_DIR/m68k-elf/bin/m68k-elf-gdb" "$BIN_DIR/m68k-elf-gdb" 2>/dev/null || true
  export MARSDEV="$MARSDEV_DIR"
  log "MARSDEV installé dans $MARSDEV_DIR"
fi

# ── SGDK ──────────────────────────────────────────────────────────────────────
step "SGDK"
SGDK_DIR="$SRC_DIR/sgdk"
if [[ ! -d "$SGDK_DIR/.git" ]]; then
  git clone https://github.com/Stephane-D/SGDK "$SGDK_DIR"
else
  git -C "$SGDK_DIR" pull --ff-only
fi
export GDK="$SGDK_DIR"
export MARSDEV="${MARSDEV:-$SRC_DIR/marsdev}"
cd "$SGDK_DIR"
# Build la lib avec debug info
make -f makelib.gen GDK="$GDK" MARSDEV="$MARSDEV" 2>&1 | tail -5
log "SGDK installé dans $SGDK_DIR"

# ── BlastEm (repo officiel Mercurial — Michael Pavone) ───────────────────────
if $INSTALL_BLASTEM; then
  step "BlastEm (hg clone — repo officiel Michael Pavone)"

  # Mercurial requis
  if ! command -v hg >/dev/null 2>&1; then
    sudo apt-get install -y --no-install-recommends mercurial
  fi

  BLASTEM_DIR="$SRC_DIR/blastem"
  HG_REPO="https://www.retrodev.com/repos/blastem"

  if [[ ! -d "$BLASTEM_DIR/.hg" ]]; then
    log "Clone du repo Mercurial $HG_REPO"
    hg clone "$HG_REPO" "$BLASTEM_DIR"
  else
    log "Mise à jour du repo Mercurial"
    hg -R "$BLASTEM_DIR" pull -u
  fi

  cd "$BLASTEM_DIR"
  # BlastEm utilise un Makefile custom — pas de configure
  # CPU_FLAGS="" pour éviter les -march trop spécifiques
  make -j"$(nproc)" CFLAGS="-O2 -g" CPU_FLAGS="" 2>&1 | tail -5
  ln -sf "$BLASTEM_DIR/blastem" "$BIN_DIR/blastem"
  log "BlastEm installé : $BIN_DIR/blastem (rev $(hg -R "$BLASTEM_DIR" id -n))"
fi

# ── ClownMDEmu ────────────────────────────────────────────────────────────────
if $INSTALL_CLOWN; then
  step "ClownMDEmu (build depuis source)"
  CLOWN_DIR="$SRC_DIR/clownmdemu"
  if [[ ! -d "$CLOWN_DIR/.git" ]]; then
    git clone --recursive https://github.com/Clownacy/clownmdemu-frontend "$CLOWN_DIR"
  else
    git -C "$CLOWN_DIR" pull --ff-only --recurse-submodules
  fi
  cd "$CLOWN_DIR"
  cmake -B build -DCMAKE_BUILD_TYPE=Release -G Ninja 2>&1 | tail -3
  cmake --build build -j"$(nproc)" 2>&1 | tail -3
  ln -sf "$CLOWN_DIR/build/clownmdemu-frontend" "$BIN_DIR/clownmdemu"
  log "ClownMDEmu installé : $BIN_DIR/clownmdemu"
fi

# ── megalink-rs ───────────────────────────────────────────────────────────────
if $INSTALL_MEGALINK; then
  step "megalink-rs (upload ED Pro)"
  source "$HOME/.cargo/env" 2>/dev/null || true
  if ! command -v megalink >/dev/null 2>&1; then
    git clone https://github.com/ricky26/megalink-rs "$SRC_DIR/megalink-rs" 2>/dev/null || \
      git -C "$SRC_DIR/megalink-rs" pull --ff-only
    cd "$SRC_DIR/megalink-rs"
    cargo build --release 2>&1 | tail -3
    cp target/release/megalink "$BIN_DIR/megalink"
  fi
  # udev rule pour ED Pro (FT232)
  cat <<'EOF' | sudo tee /etc/udev/rules.d/99-everdrive.rules >/dev/null
SUBSYSTEM=="tty", ATTRS{idVendor}=="0403", ATTRS{idProduct}=="6001", \
  MODE="0666", SYMLINK+="everdrive", TAG+="uaccess"
SUBSYSTEM=="tty", ATTRS{idVendor}=="0403", ATTRS{idProduct}=="6010", \
  MODE="0666", SYMLINK+="everdrive", TAG+="uaccess"
EOF
  sudo udevadm control --reload-rules 2>/dev/null || true
  log "megalink installé : $BIN_DIR/megalink"
fi

# ── Python deps (gdb-proxy) ───────────────────────────────────────────────────
step "Python — dépendances proxy GDB"
pip3 install --user pyserial 2>&1 | tail -2

# ── VS Code extensions ─────────────────────────────────────────────────────────
if $INSTALL_VSCODE; then
  step "Extensions VS Code"
  if command -v code >/dev/null 2>&1; then
    EXTENSIONS=(
      "zerasul.genesis-code"
      "webfreak.debug"
      "ms-vscode.cpptools"
      "ms-vscode.cpptools-extension-pack"
      "mcu-debug.debug-tracker-vscode"
    )
    for ext in "${EXTENSIONS[@]}"; do
      log "  installing $ext"
      code --install-extension "$ext" --force 2>&1 | tail -1
    done
  else
    warn "VS Code (code) non trouvé — extensions non installées."
    warn "Installe VS Code puis relance : ./setup.sh --no-blastem --no-clown --no-marsdev --no-megalink"
  fi
fi

# ── Génération du fichier d'environnement ─────────────────────────────────────
step "Génération de l'environnement"
ENV_FILE="$PREFIX/env.sh"
cat >"$ENV_FILE" <<EOF
# Megadrive Studio — source ce fichier dans ton shell (ou dans .bashrc)
# généré le $(date)
export MEGASTUDIO_PREFIX="$PREFIX"
export GDK="$SGDK_DIR"
export MARSDEV="${MARSDEV:-$SRC_DIR/marsdev}"
export BLASTEM="$BIN_DIR/blastem"
export CLOWNMDEMU="$BIN_DIR/clownmdemu"
export MEGALINK="$BIN_DIR/megalink"
export EDPRO_PORT="\${EDPRO_PORT:-/dev/everdrive}"
export PATH="$BIN_DIR:\$PATH"
EOF
log "Environnement : source $ENV_FILE"

# Ajoute au .bashrc si pas déjà présent
if ! grep -q "megadrive-studio/env.sh" "$HOME/.bashrc" 2>/dev/null; then
  echo "" >> "$HOME/.bashrc"
  echo "# Megadrive Studio" >> "$HOME/.bashrc"
  echo "source \"$ENV_FILE\"" >> "$HOME/.bashrc"
  log "Ajouté à ~/.bashrc"
fi

# ── Patch .vscode/settings.json avec les vrais chemins ────────────────────────
step "Patch settings.json avec les chemins installés"
SETTINGS=".vscode/settings.json"
if [[ -f "$SETTINGS" ]]; then
  # Remplace les placeholders par les chemins réels
  sed -i \
    -e "s|__GDK__|$SGDK_DIR|g" \
    -e "s|__MARSDEV__|${MARSDEV:-$SRC_DIR/marsdev}|g" \
    -e "s|__BLASTEM__|$BIN_DIR/blastem|g" \
    -e "s|__CLOWNMDEMU__|$BIN_DIR/clownmdemu|g" \
    "$SETTINGS"
  log "settings.json patché"
fi

# ── Résumé ────────────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}${GREEN}✓ Megadrive Studio installé${NC}"
echo ""
echo "  Toolchain  : m68k-elf-gcc/gdb dans $BIN_DIR"
echo "  SGDK       : $SGDK_DIR"
echo "  BlastEm    : $BIN_DIR/blastem"
echo "  ClownMDEmu : $BIN_DIR/clownmdemu"
echo "  megalink   : $BIN_DIR/megalink"
echo ""
echo "  → source $ENV_FILE"
echo "  → puis ouvre le dossier dans VS Code"
echo "  → Ctrl+Shift+B pour builder, F5 pour débugger"
echo ""
echo "  Pour le hardware ED Pro :"
echo "  → branche le Mega SG, EDPRO_PORT=/dev/everdrive (udev symlink)"
echo "  → make upload-edpro"
