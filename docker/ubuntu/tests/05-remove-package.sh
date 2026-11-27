# Removing a package from the bundle and re-applying removes its link and rendered file.
. "$DOTS_TEST_DIR/lib.sh"
setup basic
expect_exit 0 dots apply --yes --with-sudo
[ -L "$HOME/.aliases" ] || fail "precondition: link exists"
sed -i 's/, "shellcfg"//' "$REPO/bundles/all.toml"
grep -q shellcfg "$REPO/bundles/all.toml" && fail "fixture edit failed"
expect_exit 0 dots apply --yes --with-sudo
[ -e "$HOME/.aliases" ] || [ -L "$HOME/.aliases" ] && fail "~/.aliases not removed after package left the bundle"
[ -e "$HOME/.config/app/conf.toml" ] && fail "rendered file not removed"
expect_exit 0 dots plan --check
