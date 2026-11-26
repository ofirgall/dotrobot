#!/usr/bin/env bash
# Build dots in Docker (Ubuntu 24.04) and run the smoke suite, or apply a real repo.
#   scripts/docker-test.sh [--rev R | --worktree] [--keep] [--shell] [--repo <path>]
# Every `docker run` is capped at --memory ${DOTS_DOCKER_MEMORY:-6g} (an OOM kill of the
# container must not take the VM and its other containers with it).
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$PWD

rev=HEAD mode=rev keep=0 shell=0 repo=""
while [ $# -gt 0 ]; do
  case "$1" in
    --rev) rev="${2:?--rev needs a git rev}"; mode=rev; shift 2;;
    --worktree) mode=worktree; shift;;
    --keep) keep=1; shift;;
    --shell) shell=1; shift;;
    --repo) repo="${2:?--repo needs a path}"; shift 2;;
    -h|--help) sed -n '2,5p' "$0"; exit 0;;
    *) echo "unknown argument: $1" >&2; exit 64;;
  esac
done
if [ -n "$repo" ]; then
  repo=$(cd "$repo" && pwd) || { echo "--repo: no such directory" >&2; exit 64; }
  [ -f "$repo/dots.toml" ] || { echo "--repo: $repo has no dots.toml" >&2; exit 64; }
fi

export DOCKER_BUILDKIT=1
DF="$ROOT/docker/ubuntu/Dockerfile"
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT

if [ "$mode" = rev ]; then
  sha=$(git rev-parse --verify "$rev^{commit}")
  tag="dots-test:${sha:0:12}"
  ctx="$tmp/src"; mkdir -p "$ctx"
  git archive "$sha" | tar -x -C "$ctx"
  echo ">> building from git rev $rev ($sha) -> $tag"
else
  tag="dots-test:worktree"
  ctx="$ROOT"
  echo ">> building from the working tree -> $tag"
fi

docker build -f "$DF" --build-context "assets=$ROOT/docker" -t "$tag" "$ctx"

memory=${DOTS_DOCKER_MEMORY:-6g}
args=(run --memory "$memory" --memory-swap "$memory")
[ $keep -eq 1 ] || args+=(--rm)
name="dots-test-$$"
[ $keep -eq 0 ] || args+=(--name "$name")
[ $shell -eq 0 ] || args+=(-it)
[ -z "$repo" ] || args+=(-v "$repo:/home/dev/dots-repo:ro")
for v in DOTS_REPO_BUNDLES DOTS_REPO_PROFILE VERBOSE; do
  [ -z "${!v:-}" ] || args+=(-e "$v=${!v}")
done
if [ $shell -eq 1 ]; then cmd=(bash)
elif [ -n "$repo" ]; then cmd=(/usr/local/lib/dots-test/repo-test.sh)
else cmd=(/usr/local/lib/dots-test/run-tests.sh); fi

rc=0
docker "${args[@]}" "$tag" "${cmd[@]}" || rc=$?
[ $keep -eq 0 ] || echo ">> container kept: docker start -ai $name   (rm: docker rm $name)"
exit $rc
