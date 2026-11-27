# Sudo steps are opt-in: a plain apply skips the apt package (exit 0, with a warning), and
# --sudo-only installs it. The container has passwordless sudo.
. "$DOTS_TEST_DIR/lib.sh"
setup basic
sudo apt-get remove -y tree >/dev/null 2>&1 || true
out=$(dots apply --yes 2>&1) || fail "plain apply should exit 0 when it only skipped sudo steps"
command -v tree >/dev/null && fail "plain apply installed an apt package"
echo "$out" | grep -q "need sudo" || fail "no sudo warning: $out"
[ -L "$HOME/.aliases" ] || fail "plain apply did not apply the ordinary steps"
expect_exit 0 dots apply --yes --sudo-only
command -v tree >/dev/null || fail "--sudo-only did not install the apt package"
