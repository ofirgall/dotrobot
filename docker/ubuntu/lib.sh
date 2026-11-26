# Shared helpers, sourced by every test. Each test starts from a clean $HOME via `setup`.
FIXTURES="${DOTS_TEST_DIR:-/usr/local/lib/dots-test}/fixtures"
REPO="$HOME/work/repo"

fail() { echo "  assert failed: $*" >&2; exit 1; }

# run <expected-exit> <cmd...>: run, show output, require the exit code.
expect_exit() {
  local want="$1"; shift
  local out rc=0
  out=$("$@" 2>&1) || rc=$?
  echo "$out" | sed 's/^/    | /'
  [ "$rc" -eq "$want" ] || fail "'$*' exited $rc, wanted $want"
}

# setup <fixture> [bundles...]: fresh home state, copy the fixture, write machine.toml.
setup() {
  local fx="$1"; shift
  local bundles=("$@"); [ ${#bundles[@]} -gt 0 ] || bundles=(all)
  rm -rf "$HOME/.config" "$HOME/.local" "$HOME/.cache" "$HOME/work" \
         "$HOME/.aliases" "$HOME/.zshenv" "$HOME/.zshrc" "$HOME/.profile.dots"
  mkdir -p "$HOME/work" "$HOME/.config/dots"
  cp -r "$FIXTURES/$fx" "$REPO"
  local list; list=$(printf '"%s", ' "${bundles[@]}"); list="[${list%, }]"
  printf 'schema = 1\nroot = "%s"\nbundles = %s\n' "$REPO" "$list" > "$HOME/.config/dots/machine.toml"
}

# Set bundles in machine.toml (used to remove a package from the machine).
set_bundles() {
  local list; list=$(printf '"%s", ' "$@"); list="[${list%, }]"
  printf 'schema = 1\nroot = "%s"\nbundles = %s\n' "$REPO" "$list" > "$HOME/.config/dots/machine.toml"
}
