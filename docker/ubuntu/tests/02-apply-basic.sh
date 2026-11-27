# Apply installs an apt package + a script package, links a dotfile, renders a template; then idempotent.
. "$DOTS_TEST_DIR/lib.sh"
setup basic
sudo apt-get remove -y tree >/dev/null 2>&1 || true
command -v tree >/dev/null && fail "tree should not be installed before apply"
expect_exit 0 dots apply --yes --with-sudo
command -v tree >/dev/null || fail "apt package 'tree' not installed"
[ -x "$HOME/.local/bin/hello-dots" ] || fail "script package did not write ~/.local/bin/hello-dots"
[ "$("$HOME/.local/bin/hello-dots")" = "hello from dots" ] || fail "hello-dots output"
[ -L "$HOME/.aliases" ] || fail "~/.aliases is not a link"
grep -q "alias ll" "$HOME/.aliases" || fail "~/.aliases content"
grep -q '^os = "linux"' "$HOME/.config/app/conf.toml" || fail "template not rendered with os=linux"
expect_exit 0 dots status
expect_exit 0 dots plan --check
