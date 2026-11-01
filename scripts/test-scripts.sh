#!/usr/bin/env bash
# Run without Rust, Docker, or changes to a real workspace.
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
scratch=$(mktemp -d "${TMPDIR:-/tmp}/selfhost-scripts.XXXXXX")
trap 'rm -rf -- "$scratch"' EXIT
export SELFHOST_TEST_LOG="$scratch/commands"
mkdir -p -- "$scratch/install/bin"
cat > "$scratch/install/bin/selfhost" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
config=""
while (($#)); do
  case "$1" in
    --workspace) shift 2 ;;
    --config) config=$2; shift 2 ;;
    *) break ;;
  esac
done
[[ -z "${SELFHOST_TEST_EXPECT_CONFIG:-}" || "$config" == "$SELFHOST_TEST_EXPECT_CONFIG" ]] || exit 31
printf '%s\n' "$1" >> "$SELFHOST_TEST_LOG"
case "$1" in
  init) printf 'seed manifest\n' > "$config" ;;
  plan) exit "${SELFHOST_TEST_FAIL_PLAN:-0}" ;;
  apply) exit "${SELFHOST_TEST_FAIL_APPLY:-0}" ;;
  *) exit 1 ;;
esac
MOCK
chmod +x "$scratch/install/bin/selfhost"
binary="$scratch/install/bin/selfhost"

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

for script in setup.sh apply.sh; do
  bash -n "$script_dir/$script"
  expect_exit 0 "$script_dir/$script" --help
  expect_exit 2 "$script_dir/$script" --unknown
  expect_exit 2 "$script_dir/$script" --config
done

# Paths with spaces survive argument parsing; repeated setup never rewrites data.
workspace="$scratch/local workspace"
config="$workspace/services.yaml"
expect_exit 0 "$script_dir/setup.sh" --skip-install --install-root "$scratch/install" --workspace "$workspace"
printf 'init\nplan\n' > "$scratch/expected"
cmp "$scratch/expected" "$SELFHOST_TEST_LOG"

# Existing JSON workspaces are reused; ambiguity requires an explicit choice.
legacy="$scratch/legacy workspace"
mkdir -p -- "$legacy"
printf 'legacy manifest\n' > "$legacy/services.json"
expect_exit 0 env SELFHOST_TEST_EXPECT_CONFIG="$legacy/services.json" "$script_dir/setup.sh" --skip-install --install-root "$scratch/install" --workspace "$legacy"
[[ ! -e "$legacy/services.yaml" ]]
expect_exit 0 env SELFHOST_TEST_EXPECT_CONFIG="$legacy/services.json" "$script_dir/apply.sh" --bin "$binary" --workspace "$legacy"
printf 'second manifest\n' > "$legacy/services.yml"
expect_exit 2 "$script_dir/setup.sh" --skip-install --install-root "$scratch/install" --workspace "$legacy"
expect_exit 2 "$script_dir/apply.sh" --bin "$binary" --workspace "$legacy"
expect_exit 0 env SELFHOST_TEST_EXPECT_CONFIG="$legacy/services.json" "$script_dir/apply.sh" --bin "$binary" --workspace "$legacy" --config "$legacy/services.json"

printf 'custom manifest\n' > "$config"
cp "$config" "$scratch/original"
: > "$SELFHOST_TEST_LOG"
expect_exit 0 "$script_dir/setup.sh" --skip-install --install-root "$scratch/install" --workspace "$workspace"
cmp "$scratch/original" "$config"
printf 'plan\n' > "$scratch/expected"
cmp "$scratch/expected" "$SELFHOST_TEST_LOG"

# Planning is the default; apply only runs after a successful plan.
for mode in plan apply; do
  : > "$SELFHOST_TEST_LOG"
  apply_args=(--bin "$binary" --workspace "$workspace")
  [[ "$mode" == plan ]] || apply_args+=(--apply)
  expect_exit 0 "$script_dir/apply.sh" "${apply_args[@]}"
  printf 'plan\n' > "$scratch/expected"
  [[ "$mode" == plan ]] || printf 'apply\n' >> "$scratch/expected"
  cmp "$scratch/expected" "$SELFHOST_TEST_LOG"
done
: > "$SELFHOST_TEST_LOG"
expect_exit 23 env SELFHOST_TEST_FAIL_PLAN=23 "$script_dir/apply.sh" --bin "$binary" --workspace "$workspace" --apply
printf 'plan\n' > "$scratch/expected"
cmp "$scratch/expected" "$SELFHOST_TEST_LOG"
expect_exit 24 env SELFHOST_TEST_FAIL_APPLY=24 "$script_dir/apply.sh" --bin "$binary" --workspace "$workspace" --apply
expect_exit 23 env SELFHOST_TEST_FAIL_PLAN=23 "$script_dir/setup.sh" --skip-install --install-root "$scratch/install" --workspace "$workspace"
cmp "$scratch/original" "$config"
printf 'Script checks passed.\n'
