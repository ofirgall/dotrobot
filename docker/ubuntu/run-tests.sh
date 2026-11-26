#!/usr/bin/env bash
# Tiny runner: each tests/*.sh runs in its own bash. A test whose first lines contain
# `# XFAIL: <reason>` is an expected failure (known dots bug): XFAIL when it fails (ok),
# XPASS when it unexpectedly passes (reported, not fatal, remove the marker).
cd "$(dirname "$0")" || exit 2
export DOTS_TEST_DIR="$PWD"
pass=0 failn=0 xfail=0 xpass=0 failed=()
for t in tests/*.sh; do
  name=$(basename "$t" .sh)
  xnote=$(sed -n '1,15s/^# XFAIL: *//p' "$t" | head -1)
  log=$(mktemp)
  if bash -e -o pipefail "$t" >"$log" 2>&1; then rc=0; else rc=$?; fi
  if [ -n "$xnote" ]; then
    if [ $rc -ne 0 ]; then echo "XFAIL $name  (known bug: $xnote)"; xfail=$((xfail+1))
    else echo "XPASS $name  (marked XFAIL but passed: $xnote)"; xpass=$((xpass+1)); fi
    [ "${VERBOSE:-0}" = 1 ] || [ $rc -eq 0 ] || sed 's/^/      /' "$log"
  elif [ $rc -eq 0 ]; then
    echo "PASS  $name"; pass=$((pass+1))
    [ "${VERBOSE:-0}" = 1 ] && sed 's/^/      /' "$log"
  else
    echo "FAIL  $name"; failn=$((failn+1)); failed+=("$name"); sed 's/^/      /' "$log"
  fi
  rm -f "$log"
done
echo
echo "pass=$pass fail=$failn xfail=$xfail xpass=$xpass"
[ $failn -eq 0 ]
