#!/usr/bin/env bash
set -euo pipefail
umask 077

usage() {
  cat <<'USAGE'
Usage: scripts/setup.sh [options]
Build and install selfhost, initialize a manifest if absent, then print a plan.
No services are started. Existing manifests are never overwritten.

  --workspace DIR     State directory (default: XDG_DATA_HOME/self-host-init)
  --config FILE       Manifest (default: existing workspace manifest or services.yaml)
  --install-root DIR  Cargo installation root (default: ~/.cargo)
  --skip-install      Use the existing installed selfhost binary
  --discover          Add existing services as monitor-only during initialization
  --examples          Include disabled example services during initialization
  -h, --help          Show this help
USAGE
}

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/.." && pwd)
workspace="${XDG_DATA_HOME:-$HOME/.local/share}/self-host-init"
config=""
install_root="$HOME/.cargo"
skip_install=false
init_args=(init)
while (($#)); do
  case "$1" in
    --workspace|--config|--install-root)
      if (($# < 2)) || [[ -z "$2" ]]; then printf 'Missing value for %s\n' "$1" >&2; exit 2; fi
      case "$1" in
        --workspace) workspace=$2 ;;
        --config) config=$2 ;;
        --install-root) install_root=$2 ;;
      esac
      shift 2 ;;
    --skip-install) skip_install=true; shift ;;
    --discover|--examples) init_args+=("$1"); shift ;;
    -h|--help) usage; exit 0 ;;
    *) printf 'Unknown option: %s\n' "$1" >&2; usage >&2; exit 2 ;;
  esac
done
if [[ -z "$config" ]]; then
  for extension in yaml yml json; do
    candidate="$workspace/services.$extension"
    if [[ -e "$candidate" || -L "$candidate" ]]; then
      if [[ -n "$config" ]]; then printf 'Multiple workspace manifests found; choose one with --config.\n' >&2; exit 2; fi
      config=$candidate
    fi
  done
  config=${config:-"$workspace/services.yaml"}
fi
if [[ "$skip_install" == false ]]; then
  command -v cargo >/dev/null || { printf 'Rust and cargo are required. Install Rust, then run setup again.\n' >&2; exit 1; }
  cargo install --locked --path "$repo_root" --root "$install_root"
fi
binary="$install_root/bin/selfhost"
if [[ ! -x "$binary" ]]; then
  binary=$(command -v selfhost) || { printf 'selfhost not found. Run without --skip-install first.\n' >&2; exit 1; }
fi
mkdir -p -- "$workspace" "$(dirname -- "$config")"
if [[ -e "$config" || -L "$config" ]]; then
  printf 'Using existing manifest: %s\n' "$config"
else
  "$binary" --workspace "$workspace" --config "$config" "${init_args[@]}"
fi
"$binary" --workspace "$workspace" --config "$config" plan
printf '\nReady. Edit %s, review the plan, then run:\n' "$config"
printf '  %q --workspace %q --config %q apply\n' "$binary" "$workspace" "$config"
printf '  %q --workspace %q --config %q tui\n' "$binary" "$workspace" "$config"
