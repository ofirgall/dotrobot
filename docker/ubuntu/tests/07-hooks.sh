# A package's post_files hook runs once after its files change, not on a dry run, not again.
. "$DOTS_TEST_DIR/lib.sh"
setup hooks
rm -f "$HOME/hook-ran" "$HOME/.hookedrc"
expect_exit 0 dots plan
expect_exit 0 dots apply --dry-run
[ ! -e "$HOME/hook-ran" ] || fail "--dry-run ran a hook"
expect_exit 0 dots apply --yes --with-sudo
[ -L "$HOME/.hookedrc" ] || fail "file not linked"
[ "$(cat "$HOME/hook-ran")" = "hooked post_files" ] || fail "hook did not run with DOTS_* env"
expect_exit 0 dots apply --yes --with-sudo
[ "$(wc -l < "$HOME/hook-ran")" -eq 1 ] || fail "hook ran again with nothing changed"
