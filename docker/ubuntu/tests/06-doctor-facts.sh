# doctor runs; facts reports linux / ubuntu.
. "$DOTS_TEST_DIR/lib.sh"
setup basic
dots doctor || fail "doctor exited $?"
facts=$(dots facts --json)
echo "$facts" | jq -e . >/dev/null || fail "facts --json invalid"
[ "$(echo "$facts" | jq -r .data.facts.os.value)" = linux ] || fail "facts os != linux"
[ "$(echo "$facts" | jq -r .data.facts.distro.value)" = ubuntu ] || fail "facts distro != ubuntu"
dots facts | grep -qi ubuntu || fail "plain facts lacks ubuntu"
