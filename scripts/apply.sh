#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/apply.sh [--config FILE] [--workspace DIR] [--bin PATH] [--apply]
Validate and print the deployment plan. Add --apply to create/start managed services.
  --config FILE    Manifest (default: existing workspace manifest or services.yaml)
  --workspace DIR  State directory (default: XDG_DATA_HOME/self-host-init)
  --bin PATH       selfhost executable (default: selfhost on PATH)
  --apply          Apply enabled managed declarations after the plan succeeds
  -h, --help       Show this help
USAGE
}

workspace="${XDG_DATA_HOME:-$HOME/.local/share}/self-host-init"
config=""
binary=selfhost
apply=false
while (($#)); do
  case "$1" in
    --workspace|--config|--bin)
      if (($# < 2)) || [[ -z "$2" ]]; then printf 'Missing value for %s\n' "$1" >&2; exit 2; fi
      case "$1" in
        --workspace) workspace=$2 ;;
        --config) config=$2 ;;
        --bin) binary=$2 ;;
      esac
      shift 2 ;;
    --apply) apply=true; shift ;;
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
"$binary" --workspace "$workspace" --config "$config" plan
if [[ "$apply" == true ]]; then
  "$binary" --workspace "$workspace" --config "$config" apply
else
  printf '\nPlan only. Run again with --apply to change managed services.\n'
fi
