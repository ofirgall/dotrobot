# docker/: Ubuntu end-to-end smoke tests

Builds the real `dots` binary and runs it as a non-root `dev` user (passwordless sudo) on
Ubuntu 24.04 against real apt and the script provider. Needs Docker with BuildKit.

```
scripts/docker-test.sh                    # build git HEAD (git archive), run the suite
scripts/docker-test.sh --rev v0.2         # any git rev, reproducible
scripts/docker-test.sh --worktree         # current working tree (target/ and .git/ excluded)
scripts/docker-test.sh --shell            # bash inside the image instead of the suite
scripts/docker-test.sh --keep             # keep the container (name printed) for inspection
scripts/docker-test.sh --repo ~/dotfiles  # apply a real repo, see below
```

- `docker/ubuntu/Dockerfile`: rust builder (cargo registry/target cache mounts) -> ubuntu runtime.
  The build context is the source tree only; tests and fixtures come from a named context, so
  editing them never rebuilds Rust.
- `docker/ubuntu/tests/NN-*.sh`: one test each, started from a clean `$HOME`, run by
  `run-tests.sh` (PASS/FAIL per test, nonzero exit on failure). `VERBOSE=1` shows passing output.
  A test with `# XFAIL: <reason>` in its first lines is a known dots bug: shown as XFAIL, and
  as XPASS (non-fatal) once fixed.
- `docker/fixtures/basic/`: the fixture repo (apt `tree`, a script package, a link, a rendered template).
- **Memory limit:** every `docker run` gets `--memory 6g` (and the same `--memory-swap`, so it cannot spill into swap); set `DOTS_DOCKER_MEMORY` (for example
  `DOTS_DOCKER_MEMORY=10g scripts/docker-test.sh`) to change it. A run that exceeds the limit
  is OOM-killed inside its own container instead of taking down the VM (OrbStack) and its other
  containers. Image builds (`docker build`) are not limited; run only one at a time.
- `--repo <path>`: mounts the repo read-only at `/home/dev/dots-repo`, copies it to
  `~/work/repo`, writes machine.toml, then runs `lint`, `plan`, `apply --yes --with-sudo`, `status`, and
  `plan --check`, and prints a summary. Machine selection: `DOTS_REPO_PROFILE=name`,
  `DOTS_REPO_BUNDLES=a,b`, else the only `machines/*.toml`, else every bundle.
