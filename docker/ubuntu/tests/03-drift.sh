# Drift: a modified rendered file and a replaced link make `status` exit 2; `apply --yes` fixes them.
. "$DOTS_TEST_DIR/lib.sh"
setup basic
expect_exit 0 dots apply --yes
expect_exit 0 dots status

# rendered file edited in place
echo 'tampered = true' >> "$HOME/.config/app/conf.toml"
expect_exit 2 dots status
expect_exit 0 dots apply --yes
grep -q tampered "$HOME/.config/app/conf.toml" && fail "render drift not repaired"
expect_exit 0 dots status

# link replaced by a regular file
rm "$HOME/.aliases"; echo "mine" > "$HOME/.aliases"
expect_exit 2 dots status
expect_exit 0 dots apply --yes
[ -L "$HOME/.aliases" ] || fail "link not restored"
expect_exit 0 dots status
