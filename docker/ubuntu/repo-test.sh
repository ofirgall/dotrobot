#!/usr/bin/env bash
# Apply a real dots repo (mounted read-only at ~/dots-repo) inside the container and report.
# Env: DOTS_REPO_BUNDLES=a,b  DOTS_REPO_PROFILE=name  (default: the only machines/*.toml, else every bundle)
set -u
SRC="$HOME/dots-repo"
WORK="$HOME/work/repo"
[ -f "$SRC/dots.toml" ] || { echo "no dots.toml in $SRC (is --repo a dots repo?)" >&2; exit 2; }

mkdir -p "$HOME/work" "$HOME/.config/dots"
rm -rf "$WORK"
cp -a "$SRC" "$WORK" || { echo "copy to $WORK failed" >&2; exit 2; }

mfile="$HOME/.config/dots/machine.toml"
if [ -n "${DOTS_REPO_PROFILE:-}" ]; then
  printf 'schema = 1\nroot = "%s"\nprofile = "%s"\n' "$WORK" "$DOTS_REPO_PROFILE" > "$mfile"
  how="profile $DOTS_REPO_PROFILE"
elif [ -n "${DOTS_REPO_BUNDLES:-}" ]; then
  list=$(echo "$DOTS_REPO_BUNDLES" | sed 's/[^,]\+/"&"/g')
  printf 'schema = 1\nroot = "%s"\nbundles = [%s]\n' "$WORK" "$list" > "$mfile"
  how="bundles $DOTS_REPO_BUNDLES"
else
  mapfile -t profs < <(ls "$WORK"/machines/*.toml 2>/dev/null)
  if [ ${#profs[@]} -eq 1 ]; then
    p=$(basename "${profs[0]}" .toml)
    printf 'schema = 1\nroot = "%s"\nprofile = "%s"\n' "$WORK" "$p" > "$mfile"
    how="profile $p (only one in machines/)"
  else
    mapfile -t bs < <(ls "$WORK"/bundles/*.toml 2>/dev/null | xargs -r -n1 basename | sed 's/\.toml$//')
    list=$(printf '"%s", ' "${bs[@]}"); list="${list%, }"
    printf 'schema = 1\nroot = "%s"\nbundles = [%s]\n' "$WORK" "$list" > "$mfile"
    how="all bundles: ${bs[*]:-none}"
  fi
fi
echo "== machine.toml ($how)"; sed 's/^/   /' "$mfile"

step() { # step <label> <cmd...>; records exit code in R_<label>
  local label="$1"; shift
  echo; echo "== $label: $*"
  "$@"; local rc=$?
  eval "R_$label=$rc"; echo "-- exit $rc"
}

step lint dots lint
step plan dots plan
step apply dots apply --yes --with-sudo
step status dots status
step recheck dots plan --check

echo
echo "================ SUMMARY ================"
printf '%-8s %s\n' lint "exit $R_lint" plan "exit $R_plan" apply "exit $R_apply (0 = ok)" \
  status "exit $R_status (0 = clean, 2 = drift)" recheck "exit $R_recheck (0 = idempotent, 3 = plan not empty)"
rc=0
[ "$R_apply" -eq 0 ] || { echo "RESULT: FAIL apply"; rc=1; }
[ "$R_status" -eq 0 ] || { echo "RESULT: FAIL status (drift after apply)"; rc=1; }
[ "$R_recheck" -eq 0 ] || { echo "RESULT: FAIL not idempotent"; rc=1; }
[ $rc -eq 0 ] && echo "RESULT: OK"
exit $rc
