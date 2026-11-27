# dots status --json is valid JSON with the documented schema id.
. "$DOTS_TEST_DIR/lib.sh"
setup basic
dots apply --yes --with-sudo >/dev/null
dots status --json | jq -e . >/dev/null || fail "status --json is not valid JSON"
[ "$(dots status --json | jq -r .schema)" = "dots/status@1" ] || fail "schema != dots/status@1"
