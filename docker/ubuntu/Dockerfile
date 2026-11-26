# syntax=docker/dockerfile:1.7
# Build context: the dots source tree (docker/ is excluded by Dockerfile.dockerignore).
# Named context "assets": the docker/ directory (tests, fixtures, runner), so editing a test
# never invalidates the Rust build layer.
ARG RUST_VERSION=1.91

FROM rust:${RUST_VERSION}-bookworm AS builder
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --release -p dots-cli --bin dots \
 && install -D target/release/dots /out/dots

FROM ubuntu:24.04 AS runtime
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      sudo git curl ca-certificates gpg xz-utils jq less \
 && rm -f /etc/apt/apt.conf.d/docker-clean \
 && (userdel -r ubuntu 2>/dev/null || true) \
 && useradd -m -s /bin/bash -u 1000 dev \
 && echo 'dev ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/dev \
 && chmod 0440 /etc/sudoers.d/dev
# apt lists are kept on purpose: the apt provider installs, it does not `apt-get update`.
COPY --from=builder /out/dots /usr/local/bin/dots
COPY --from=assets ubuntu/lib.sh ubuntu/run-tests.sh ubuntu/repo-test.sh /usr/local/lib/dots-test/
COPY --from=assets ubuntu/tests /usr/local/lib/dots-test/tests
COPY --from=assets fixtures /usr/local/lib/dots-test/fixtures
RUN chmod -R a+rX /usr/local/lib/dots-test && chmod a+x /usr/local/lib/dots-test/*.sh
ENV DOTS_TEST_DIR=/usr/local/lib/dots-test
ENV PATH=/home/dev/.local/bin:$PATH
USER dev
WORKDIR /home/dev
CMD ["/usr/local/lib/dots-test/run-tests.sh"]
