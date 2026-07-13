#!/bin/sh
# Install sidecar — a side-by-side diff/review TUI.
#
#   curl -fsSL https://raw.githubusercontent.com/ftorrresd/sidecar/main/scripts/install.sh | sh
#
# Environment overrides:
#   SIDECAR_VERSION       tag to install (default: latest release)
#   SIDECAR_BIN_DIR       install directory (default: $HOME/.local/bin)
#   SIDECAR_INSTALL_DEPS  1 = auto-install missing deps, 0 = never (default: ask)
#   GITHUB_TOKEN          optional token for GitHub API rate limits

set -eu

REPO="ftorrresd/sidecar"
BIN="sidecar"

info() { printf '\033[1;36m==>\033[0m %s\n' "$1"; }
warn() { printf '\033[1;33mnote:\033[0m %s\n' "$1"; }
err() {
	printf '\033[1;31merror:\033[0m %s\n' "$1" >&2
	exit 1
}

# ---- Detect platform --------------------------------------------------------
os=$(uname -s)
arch=$(uname -m)
case "$os" in
Linux) os=linux ;;
Darwin) os=macos ;;
*) err "unsupported operating system: $os" ;;
esac
case "$arch" in
x86_64 | amd64) arch=x86_64 ;;
arm64 | aarch64) arch=aarch64 ;;
*) err "unsupported architecture: $arch" ;;
esac
asset="${BIN}-${os}-${arch}.tar.gz"

# Platform identifiers for the various dependency release-asset conventions:
#   triple    — Rust target (delta, bat, yazi)
#   rg_triple — ripgrep prefers musl on Linux
#   goos/goarch — Go-style names (fzf)
triple=""
rg_triple=""
case "$os/$arch" in
linux/x86_64)
	triple=x86_64-unknown-linux-gnu
	rg_triple=x86_64-unknown-linux-musl
	;;
linux/aarch64)
	triple=aarch64-unknown-linux-gnu
	rg_triple=aarch64-unknown-linux-gnu
	;;
macos/x86_64)
	triple=x86_64-apple-darwin
	rg_triple=x86_64-apple-darwin
	;;
macos/aarch64)
	triple=aarch64-apple-darwin
	rg_triple=aarch64-apple-darwin
	;;
esac
case "$os" in linux) goos=linux ;; macos) goos=darwin ;; esac
case "$arch" in x86_64) goarch=amd64 ;; aarch64) goarch=arm64 ;; esac

# ---- Downloader -------------------------------------------------------------
if command -v curl >/dev/null 2>&1; then
	fetch() { curl -fsSL "$1"; }
	fetch_to() { curl -fsSL "$1" -o "$2"; }
	fetch_api() {
		if [ -n "${GITHUB_TOKEN:-}" ]; then
			curl -fsSL -H "Authorization: Bearer ${GITHUB_TOKEN}" "$1"
		else
			curl -fsSL "$1"
		fi
	}
elif command -v wget >/dev/null 2>&1; then
	fetch() { wget -qO- "$1"; }
	fetch_to() { wget -qO "$2" "$1"; }
	fetch_api() {
		if [ -n "${GITHUB_TOKEN:-}" ]; then
			wget -qO- --header="Authorization: Bearer ${GITHUB_TOKEN}" "$1"
		else
			wget -qO- "$1"
		fi
	}
else
	err "need either curl or wget installed"
fi

# The latest release tag of a GitHub repo (e.g. "0.18.2" or "v0.24.0").
gh_latest_tag() {
	repo=$1
	tag=$(fetch_api "https://api.github.com/repos/${repo}/releases/latest" |
		grep '"tag_name"' | head -n1 |
		sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')
	[ -n "$tag" ] || {
		warn "  could not determine latest release for ${repo}" >&2
		return 1
	}
	printf '%s\n' "$tag"
}

# Prompt for yes/no (reading the real terminal, since stdin may be the pipe).
# SIDECAR_INSTALL_DEPS forces the answer for non-interactive installs.
confirm() {
	case "${SIDECAR_INSTALL_DEPS:-}" in
	1 | y | Y | yes) return 0 ;;
	0 | n | N | no) return 1 ;;
	esac
	[ -r /dev/tty ] || return 1
	printf '%s [y/N] ' "$1" >/dev/tty
	IFS= read -r ans </dev/tty || ans=""
	case "$ans" in y | Y | yes | YES) return 0 ;; *) return 1 ;; esac
}

# ---- Install location -------------------------------------------------------
bindir="${SIDECAR_BIN_DIR:-$HOME/.local/bin}"
mkdir -p "$bindir"

# ---- Install sidecar itself -------------------------------------------------
# Skipped when SIDECAR_DEPS_ONLY=1 (used by CI to test just the dependency
# installation without a published sidecar release).
if [ "${SIDECAR_DEPS_ONLY:-0}" != 1 ]; then
	tag="${SIDECAR_VERSION:-}"
	if [ -z "$tag" ]; then
		info "Finding the latest sidecar release..."
		tag=$(gh_latest_tag "$REPO") || err "could not determine the latest release (set SIDECAR_VERSION to override)"
	fi

	url="https://github.com/${REPO}/releases/download/${tag}/${asset}"
	tmp=$(mktemp -d)
	trap 'rm -rf "$tmp"' EXIT INT TERM

	info "Downloading ${asset} (${tag})..."
	fetch_to "$url" "$tmp/$asset" || err "download failed: $url"
	tar -xzf "$tmp/$asset" -C "$tmp" || err "failed to extract $asset"
	[ -f "$tmp/$BIN" ] || err "archive did not contain the '$BIN' binary"
	chmod +x "$tmp/$BIN"
	mv -f "$tmp/$BIN" "$bindir/$BIN"
	info "Installed sidecar to $bindir/$BIN"
fi

case ":$PATH:" in
*":$bindir:"*) : ;;
*) warn "$bindir is not on your PATH. Add it, e.g.: export PATH=\"$bindir:\$PATH\"" ;;
esac

# ---- Runtime dependencies ---------------------------------------------------
# Download `url`, extract it, find the `name` binary inside, and install it.
# `typ` is tar or zip.
install_archive() {
	url=$1
	name=$2
	typ=$3
	dir=$(mktemp -d)
	info "  downloading ${name}..."
	if ! fetch_to "$url" "$dir/pkg"; then
		rm -rf "$dir"
		warn "  could not download ${name} from ${url}"
		return 1
	fi
	case "$typ" in
	tar) tar -xzf "$dir/pkg" -C "$dir" 2>/dev/null || {
		rm -rf "$dir"
		warn "  failed to extract ${name}"
		return 1
	} ;;
	zip)
		if ! command -v unzip >/dev/null 2>&1; then
			rm -rf "$dir"
			warn "  need 'unzip' to install ${name}"
			return 1
		fi
		unzip -q "$dir/pkg" -d "$dir" || {
			rm -rf "$dir"
			warn "  failed to extract ${name}"
			return 1
		}
		;;
	esac
	src=$(find "$dir" -type f -name "$name" | head -n1)
	if [ -z "$src" ]; then
		rm -rf "$dir"
		warn "  ${name} binary not found in its archive"
		return 1
	fi
	chmod +x "$src"
	mv -f "$src" "$bindir/$name"
	rm -rf "$dir"
	info "  installed ${name} to ${bindir}/${name}"
}

# Per-tool installers (each release names its assets differently).
install_delta() {
	t=$(gh_latest_tag dandavison/delta) || return 1
	install_archive "https://github.com/dandavison/delta/releases/download/${t}/delta-${t}-${triple}.tar.gz" delta tar
}
install_bat() {
	t=$(gh_latest_tag sharkdp/bat) || return 1
	install_archive "https://github.com/sharkdp/bat/releases/download/${t}/bat-${t}-${triple}.tar.gz" bat tar
}
install_rg() {
	t=$(gh_latest_tag BurntSushi/ripgrep) || return 1
	install_archive "https://github.com/BurntSushi/ripgrep/releases/download/${t}/ripgrep-${t}-${rg_triple}.tar.gz" rg tar
}
install_fzf() {
	t=$(gh_latest_tag junegunn/fzf) || return 1
	install_archive "https://github.com/junegunn/fzf/releases/download/${t}/fzf-${t#v}-${goos}_${goarch}.tar.gz" fzf tar
}
install_yazi() {
	t=$(gh_latest_tag sxyazi/yazi) || return 1
	install_archive "https://github.com/sxyazi/yazi/releases/download/${t}/yazi-${triple}.zip" yazi zip
}

# Offer to install a missing tool. Args: command-name  label  installer-fn  note.
# SIDECAR_FORCE_DEPS=1 installs even when the tool is already present (CI test).
offer_install() {
	cmd=$1
	label=$2
	fn=$3
	note=$4
	if [ "${SIDECAR_FORCE_DEPS:-0}" != 1 ]; then
		command -v "$cmd" >/dev/null 2>&1 && return 0
	fi
	if confirm "${label} is not installed. Install the latest from GitHub?"; then
		"$fn" || warn "  ${note}"
	else
		warn "${label} — ${note}"
	fi
}

# Required renderers.
offer_install delta delta install_delta "needed at runtime: https://github.com/dandavison/delta/releases"
offer_install bat bat install_bat "needed at runtime: https://github.com/sharkdp/bat/releases"
# On-demand tools (used by specific keys).
offer_install rg ripgrep install_rg "used by search: https://github.com/BurntSushi/ripgrep/releases"
offer_install fzf fzf install_fzf "used by the file/diff pickers: https://github.com/junegunn/fzf/releases"
offer_install yazi yazi install_yazi "used by the yazi file picker: https://github.com/sxyazi/yazi/releases"

# git has no convenient single-binary release — use the system package manager.
command -v git >/dev/null 2>&1 ||
	warn "git is not installed — install it with your package manager (e.g. 'pacman -S git' or 'brew install git')"

info "Done — run 'sidecar' inside a git repository."
