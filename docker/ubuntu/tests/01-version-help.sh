# dots --version and --help list the commands.
. "$DOTS_TEST_DIR/lib.sh"
dots --version | grep -E '^dots [0-9]+\.[0-9]+' || fail "--version"
help=$(dots --help)
for c in init apply plan status doctor facts restore; do
  echo "$help" | grep -qE "^ +$c( |$)" || fail "--help does not list '$c'"
done
