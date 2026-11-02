#!/usr/bin/env bash

init_default_workspace() {
  local base="${XDG_DATA_HOME:-$HOME/.local/share}" canonical legacy
  [[ "$base" = /* ]] || { printf 'XDG_DATA_HOME must be absolute; choose --workspace explicitly.\n' >&2; return 2; }
  canonical="$base/init"
  legacy="$base/self-host-init"
  if [[ -e "$canonical" || -L "$canonical" ]] && [[ -e "$legacy" || -L "$legacy" ]]; then
    [[ "$canonical" -ef "$legacy" ]] || { printf 'Both Init and legacy workspaces exist; choose --workspace explicitly.\n' >&2; return 2; }
  elif [[ -e "$legacy" || -L "$legacy" ]]; then
    canonical=$legacy
  fi
  printf '%s\n' "$canonical"
}

init_system_binary() {
  local candidate=$1 system_binary parent target links=0
  while :; do
    while [[ "$candidate" == *//* ]]; do candidate=${candidate//\/\//\/}; done
    case "$candidate" in /bin/init|/sbin/init|/usr/bin/init|/usr/sbin/init|*/sbin/init) return 0 ;; esac
    parent=$(cd -P -- "$(dirname -- "$candidate")" 2>/dev/null && pwd) || parent=""
    [[ -z "$parent" ]] || candidate="$parent/${candidate##*/}"
    case "$candidate" in /bin/init|/sbin/init|/usr/bin/init|/usr/sbin/init|*/sbin/init) return 0 ;; esac
    [[ -L "$candidate" ]] || break
    links=$((links + 1))
    [[ "$links" -le 40 ]] || return 0
    target=$(readlink "$candidate") || return 0
    if [[ "$target" = /* ]]; then candidate=$target; else candidate="$parent/$target"; fi
  done
  for system_binary in /sbin/init /usr/sbin/init /usr/lib/systemd/systemd /lib/systemd/systemd; do
    if [[ -e "$system_binary" && "$candidate" -ef "$system_binary" ]]; then return 0; fi
  done
  return 1
}

init_script_binary() {
  local candidate identity
  for candidate in "$@"; do
    [[ "$candidate" != -* ]] || continue
    candidate=$(command -v "$candidate" 2>/dev/null) || continue
    [[ -f "$candidate" && -x "$candidate" ]] || continue
    init_system_binary "$candidate" && continue
    identity=$("$candidate" --version 2>/dev/null) || continue
    if [[ "$identity" =~ ^(init|selfhost)[[:space:]][0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.+-]+)?$ ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  printf 'Init was not found. Install its user-local init executable or choose --bin explicitly. The operating system init is not supported.\n' >&2
  return 1
}
