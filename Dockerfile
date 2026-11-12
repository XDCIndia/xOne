# Image of the release artifact (XB-26). It contains the exact binary that ci/release.sh built,
# checksummed and signed; it does not compile anything. Build context is dist/:
#   make release && make docker
# Base: Ubuntu 24.04, the OS the release binary is built on (it links glibc dynamically).
# Pinned by digest; update the digest and the date together:
#   docker pull ubuntu:24.04 && docker image inspect ubuntu:24.04 --format '{{index .RepoDigests 0}}'
ARG BASE=ubuntu:24.04@sha256:008173c23f95b170204355c12626cb5a965d779a7e1283b09e9cffbb1bf33ca3

FROM ${BASE} AS unpack
ARG VERSION
WORKDIR /unpack
COPY xone-${VERSION}-linux-x86_64.tar.gz SHA256SUMS ./
# Refuse an artifact that does not match SHA256SUMS (the signature is checked by the operator
# on the host, see docs/ops/release.md).
RUN grep " xone-${VERSION}-linux-x86_64.tar.gz\$" SHA256SUMS | sha256sum -c - \
 && tar -xzf xone-${VERSION}-linux-x86_64.tar.gz \
 && mv xone-${VERSION}-linux-x86_64 /opt-xone

FROM ${BASE}
ARG VERSION
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home-dir /data --shell /usr/sbin/nologin xone \
 && install -d -o xone -g xone -m 0700 /data
COPY --from=unpack /opt-xone /opt/xone
ENV XONE_BIN=/opt/xone/xone DATADIR=/data
USER xone
VOLUME /data
# P2P (tcp+udp), HTTP-RPC, Engine API. Secrets (ETHSTATS_SECRET) are passed with `docker run --env-file`.
EXPOSE 30303 30303/udp 8545 8551
LABEL org.opencontainers.image.title="xone" org.opencontainers.image.version="${VERSION}"
ENTRYPOINT ["/opt/xone/run-xone.sh"]
