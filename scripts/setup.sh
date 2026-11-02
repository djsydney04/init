#!/usr/bin/env bash
set -euo pipefail
umask 077

usage() {
  cat <<'USAGE'
Usage: scripts/setup.sh [options]
Build and install Init, initialize a manifest if absent, then print a plan.
No services are started. Existing manifests are never overwritten.

  --workspace DIR     State directory (default: XDG_DATA_HOME/init; existing legacy workspace is reused)
  --config FILE       Manifest (default: existing workspace manifest or services.yaml)
  --install-root DIR  Cargo installation root (default: ~/.cargo)
  --skip-install      Use the existing installed Init binary
  --discover          Add existing services as monitor-only during initialization
  --examples          Include disabled example services during initialization
  -h, --help          Show this help
USAGE
}

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "$script_dir/common.sh"
repo_root=$(cd -- "$script_dir/.." && pwd)
workspace=""
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
workspace=${workspace:-$(init_default_workspace)}
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
  if init_system_binary "$install_root/bin/init"; then printf 'Choose a user installation root; Init cannot replace the operating system init.\n' >&2; exit 2; fi
  for name in init selfhost; do
    candidate="$install_root/bin/$name"
    if [[ -e "$candidate" || -L "$candidate" ]]; then
      init_script_binary "$candidate" >/dev/null || { printf 'Refusing to replace %s; choose another --install-root.\n' "$candidate" >&2; exit 2; }
    fi
  done
  command -v cargo >/dev/null || { printf 'Rust and cargo are required. Install Rust, then run setup again.\n' >&2; exit 1; }
  cargo install --locked --force --path "$repo_root" --root "$install_root"
fi
binary=$(init_script_binary "$install_root/bin/init" "$install_root/bin/selfhost" init selfhost)
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
