#!/usr/bin/env bash
# Run without Rust, Docker, or changes to a real workspace.
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
scratch=$(mktemp -d "${TMPDIR:-/tmp}/init-scripts.XXXXXX")
trap 'rm -rf -- "$scratch"' EXIT
export INIT_TEST_LOG="$scratch/commands"
mkdir -p -- "$scratch/install/bin"
cat > "$scratch/install/bin/init" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == --version ]]; then printf 'init 0.5.0\n'; exit 0; fi
config=""
workspace=""
while (($#)); do
  case "$1" in
    --workspace) workspace=$2; shift 2 ;;
    --config) config=$2; shift 2 ;;
    *) break ;;
  esac
done
[[ -z "${INIT_TEST_EXPECT_WORKSPACE:-}" || "$workspace" == "$INIT_TEST_EXPECT_WORKSPACE" ]] || exit 32
[[ -z "${INIT_TEST_EXPECT_CONFIG:-}" || "$config" == "$INIT_TEST_EXPECT_CONFIG" ]] || exit 31
printf '%s\n' "$1" >> "$INIT_TEST_LOG"
case "$1" in
  init) printf 'seed manifest\n' > "$config" ;;
  plan) exit "${INIT_TEST_FAIL_PLAN:-0}" ;;
  apply) exit "${INIT_TEST_FAIL_APPLY:-0}" ;;
  *) exit 1 ;;
esac
MOCK
chmod +x "$scratch/install/bin/init"
binary="$scratch/install/bin/init"

expect_exit() {
  local expected=$1 actual=0
  shift
  "$@" > "$scratch/output" 2>&1 || actual=$?
  if [[ "$actual" != "$expected" ]]; then
    cat "$scratch/output" >&2
    printf 'Expected exit %s, got %s: %s\n' "$expected" "$actual" "$*" >&2
    exit 1
  fi
}

for script in common.sh setup.sh apply.sh test-scripts.sh; do
  bash -n "$script_dir/$script"
done
for script in setup.sh apply.sh; do
  expect_exit 0 "$script_dir/$script" --help
  expect_exit 2 "$script_dir/$script" --unknown
  expect_exit 2 "$script_dir/$script" --config
done

# Paths with spaces survive argument parsing; repeated setup never rewrites data.
workspace="$scratch/local workspace"
config="$workspace/services.yaml"
expect_exit 0 "$script_dir/setup.sh" --skip-install --install-root "$scratch/install" --workspace "$workspace"
printf 'init\nplan\n' > "$scratch/expected"
cmp "$scratch/expected" "$INIT_TEST_LOG"

# Existing JSON workspaces are reused; ambiguity requires an explicit choice.
legacy="$scratch/legacy workspace"
mkdir -p -- "$legacy"
printf 'legacy manifest\n' > "$legacy/services.json"
expect_exit 0 env INIT_TEST_EXPECT_CONFIG="$legacy/services.json" "$script_dir/setup.sh" --skip-install --install-root "$scratch/install" --workspace "$legacy"
[[ ! -e "$legacy/services.yaml" ]]
expect_exit 0 env INIT_TEST_EXPECT_CONFIG="$legacy/services.json" "$script_dir/apply.sh" --bin "$binary" --workspace "$legacy"
printf 'second manifest\n' > "$legacy/services.yml"
expect_exit 2 "$script_dir/setup.sh" --skip-install --install-root "$scratch/install" --workspace "$legacy"
expect_exit 2 "$script_dir/apply.sh" --bin "$binary" --workspace "$legacy"
expect_exit 0 env INIT_TEST_EXPECT_CONFIG="$legacy/services.json" "$script_dir/apply.sh" --bin "$binary" --workspace "$legacy" --config "$legacy/services.json"

printf 'custom manifest\n' > "$config"
cp "$config" "$scratch/original"
: > "$INIT_TEST_LOG"
expect_exit 0 "$script_dir/setup.sh" --skip-install --install-root "$scratch/install" --workspace "$workspace"
cmp "$scratch/original" "$config"
printf 'plan\n' > "$scratch/expected"
cmp "$scratch/expected" "$INIT_TEST_LOG"

# Planning is the default; apply only runs after a successful plan.
for mode in plan apply; do
  : > "$INIT_TEST_LOG"
  apply_args=(--bin "$binary" --workspace "$workspace")
  [[ "$mode" == plan ]] || apply_args+=(--apply)
  expect_exit 0 "$script_dir/apply.sh" "${apply_args[@]}"
  printf 'plan\n' > "$scratch/expected"
  [[ "$mode" == plan ]] || printf 'apply\n' >> "$scratch/expected"
  cmp "$scratch/expected" "$INIT_TEST_LOG"
done
: > "$INIT_TEST_LOG"
expect_exit 23 env INIT_TEST_FAIL_PLAN=23 "$script_dir/apply.sh" --bin "$binary" --workspace "$workspace" --apply
printf 'plan\n' > "$scratch/expected"
cmp "$scratch/expected" "$INIT_TEST_LOG"
expect_exit 24 env INIT_TEST_FAIL_APPLY=24 "$script_dir/apply.sh" --bin "$binary" --workspace "$workspace" --apply
expect_exit 23 env INIT_TEST_FAIL_PLAN=23 "$script_dir/setup.sh" --skip-install --install-root "$scratch/install" --workspace "$workspace"
cmp "$scratch/original" "$config"

fresh="$scratch/fresh data"
: > "$INIT_TEST_LOG"
expect_exit 0 env XDG_DATA_HOME="$fresh" INIT_TEST_EXPECT_WORKSPACE="$fresh/init" "$script_dir/setup.sh" --skip-install --install-root "$scratch/install"
printf 'seed manifest\n' > "$scratch/expected"
cmp "$scratch/expected" "$fresh/init/services.yaml"
[[ ! -e "$fresh/self-host-init" ]]
expect_exit 0 env XDG_DATA_HOME="$fresh" INIT_TEST_EXPECT_WORKSPACE="$fresh/init" "$script_dir/apply.sh" --bin "$binary"

old_data="$scratch/old data"
old_workspace="$old_data/self-host-init"
mkdir -p -- "$old_workspace"
printf 'preserve the legacy declaration\n' > "$old_workspace/services.json"
cp "$old_workspace/services.json" "$scratch/legacy-original"
: > "$INIT_TEST_LOG"
expect_exit 0 env XDG_DATA_HOME="$old_data" INIT_TEST_EXPECT_WORKSPACE="$old_workspace" INIT_TEST_EXPECT_CONFIG="$old_workspace/services.json" "$script_dir/setup.sh" --skip-install --install-root "$scratch/install"
expect_exit 0 env XDG_DATA_HOME="$old_data" INIT_TEST_EXPECT_WORKSPACE="$old_workspace" "$script_dir/apply.sh" --bin "$binary"
printf 'plan\nplan\n' > "$scratch/expected"
cmp "$scratch/expected" "$INIT_TEST_LOG"
cmp "$scratch/legacy-original" "$old_workspace/services.json"
[[ ! -e "$old_data/init" ]]

mkdir -- "$old_data/init"
: > "$INIT_TEST_LOG"
expect_exit 2 env XDG_DATA_HOME="$old_data" "$script_dir/setup.sh" --skip-install --install-root "$scratch/install"
expect_exit 2 env XDG_DATA_HOME="$old_data" "$script_dir/apply.sh" --bin "$binary" --apply
[[ ! -s "$INIT_TEST_LOG" ]]
expect_exit 0 env XDG_DATA_HOME="$old_data" INIT_TEST_EXPECT_WORKSPACE="$old_workspace" "$script_dir/setup.sh" --skip-install --install-root "$scratch/install" --workspace "$old_workspace"
expect_exit 0 env XDG_DATA_HOME="$old_data" INIT_TEST_EXPECT_WORKSPACE="$old_workspace" "$script_dir/apply.sh" --bin "$binary" --workspace "$old_workspace"
cmp "$scratch/legacy-original" "$old_workspace/services.json"

linked_data="$scratch/linked data"
mkdir -p -- "$linked_data/self-host-init"
cp "$scratch/legacy-original" "$linked_data/self-host-init/services.json"
ln -s "self-host-init" "$linked_data/init"
expect_exit 0 env XDG_DATA_HOME="$linked_data" INIT_TEST_EXPECT_WORKSPACE="$linked_data/init" "$script_dir/setup.sh" --skip-install --install-root "$scratch/install"
expect_exit 0 env XDG_DATA_HOME="$linked_data" INIT_TEST_EXPECT_WORKSPACE="$linked_data/init" "$script_dir/apply.sh" --bin "$binary"
cmp "$scratch/legacy-original" "$linked_data/self-host-init/services.json"
expect_exit 2 env XDG_DATA_HOME=relative "$script_dir/apply.sh" --bin "$binary"

legacy_install="$scratch/legacy install"
mkdir -p -- "$legacy_install/bin"
sed 's/init 0\.5\.0/selfhost 0.4.0/' "$binary" > "$legacy_install/bin/selfhost"
chmod +x "$legacy_install/bin/selfhost"
expect_exit 0 "$script_dir/setup.sh" --skip-install --install-root "$legacy_install" --workspace "$workspace"
: > "$INIT_TEST_LOG"
expect_exit 0 env PATH="$legacy_install/bin:/usr/bin:/bin:/usr/sbin:/sbin" "$script_dir/apply.sh" --workspace "$workspace" --apply
printf 'plan\napply\n' > "$scratch/expected"
cmp "$scratch/expected" "$INIT_TEST_LOG"

export INIT_TEST_UNTRUSTED_LOG="$scratch/untrusted-commands"
mkdir -p -- "$scratch/unrelated/bin" "$scratch/system/sbin" "$scratch/tools" "$scratch/install-alias"
cat > "$scratch/unrelated/bin/init" <<'UNRELATED'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$INIT_TEST_UNTRUSTED_LOG"
printf 'unrelated program 1.0.0\n'
UNRELATED
cat > "$scratch/system/sbin/init" <<'SYSTEM'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$INIT_TEST_UNTRUSTED_LOG"
printf 'init 0.5.0\n'
SYSTEM
cat > "$scratch/tools/cargo" <<'CARGO'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$INIT_TEST_UNTRUSTED_LOG"
if [[ -n "${INIT_TEST_CARGO_INSTALL_SOURCE:-}" ]]; then
  root=""
  force=false
  while (($#)); do
    case "$1" in
      --root) root=$2; shift 2 ;;
      --force) force=true; shift ;;
      *) shift ;;
    esac
  done
  [[ "$force" == true && -n "$root" ]] || exit 91
  cp "$INIT_TEST_CARGO_INSTALL_SOURCE" "$root/bin/init"
  cp "$INIT_TEST_CARGO_INSTALL_SOURCE" "$root/bin/selfhost"
  exit 0
fi
exit 90
CARGO
chmod +x "$scratch/unrelated/bin/init" "$scratch/system/sbin/init" "$scratch/tools/cargo"
safe_path="$scratch/tools:/usr/bin:/bin:/usr/sbin:/sbin"
: > "$INIT_TEST_LOG"
expect_exit 1 "$script_dir/apply.sh" --bin "$scratch/unrelated/bin/init" --workspace "$workspace" --apply
expect_exit 1 env PATH="$safe_path" "$script_dir/setup.sh" --skip-install --install-root "$scratch/unrelated" --workspace "$workspace"
printf '%s\n' --version --version > "$scratch/expected"
cmp "$scratch/expected" "$INIT_TEST_UNTRUSTED_LOG"
[[ ! -s "$INIT_TEST_LOG" ]]

ln -s "$scratch/system/sbin" "$scratch/install-alias/bin"
ln -s "$scratch/system/sbin/init" "$scratch/system-init-link"
: > "$INIT_TEST_UNTRUSTED_LOG"
for unsafe in "$scratch/system/sbin/init" "$scratch/system-init-link" "$scratch/install-alias/bin/init"; do
  expect_exit 1 "$script_dir/apply.sh" --bin "$unsafe" --workspace "$workspace" --apply
done
for unsafe_root in / // /usr /usr/.. "$scratch/install-alias"; do
  expect_exit 2 env PATH="$safe_path" "$script_dir/setup.sh" --install-root "$unsafe_root" --workspace "$workspace"
done
[[ ! -s "$INIT_TEST_UNTRUSTED_LOG" ]]
[[ ! -s "$INIT_TEST_LOG" ]]

# A Cargo package rename needs --force, but only our own targets may be replaced.
: > "$INIT_TEST_UNTRUSTED_LOG"
expect_exit 0 env PATH="$safe_path" INIT_TEST_CARGO_INSTALL_SOURCE="$binary" "$script_dir/setup.sh" --install-root "$legacy_install" --workspace "$workspace"
[[ $("$legacy_install/bin/init" --version) == 'init 0.5.0' ]]
[[ $("$legacy_install/bin/selfhost" --version) == 'init 0.5.0' ]]
: > "$INIT_TEST_LOG"
: > "$INIT_TEST_UNTRUSTED_LOG"
expect_exit 2 env PATH="$safe_path" "$script_dir/setup.sh" --install-root "$scratch/unrelated" --workspace "$workspace"
printf '%s\n' --version > "$scratch/expected"
cmp "$scratch/expected" "$INIT_TEST_UNTRUSTED_LOG"
[[ ! -s "$INIT_TEST_LOG" ]]

source "$script_dir/common.sh"
for system_binary in /sbin/init /usr/sbin/init /usr/lib/systemd/systemd /lib/systemd/systemd; do
  if [[ -e "$system_binary" ]]; then
    ln -s "$system_binary" "$scratch/os-init-link"
    init_system_binary "$scratch/os-init-link"
    rm -- "$scratch/os-init-link"
  fi
done
printf 'Script checks passed.\n'
