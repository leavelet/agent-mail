# syntax=docker/dockerfile:1
#
# amail server image: the web interface is built first, then embedded into
# the release binary, which runs on a slim Debian base as a non-root user.
#
#   docker build -t amail .
#   docker run -p 127.0.0.1:8025:8025 -v amail-data:/data amail
#
# The base images can be replaced (for a registry mirror) with build
# arguments, e.g. --build-arg RUNTIME_IMAGE=mirror.example/library/debian:bookworm-slim

ARG NODE_IMAGE=node:22-bookworm-slim
ARG RUST_IMAGE=rust:1-bookworm
ARG RUNTIME_IMAGE=debian:bookworm-slim

# --- the web interface -----------------------------------------------------------
FROM ${NODE_IMAGE} AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
RUN npm run build

# --- the binary --------------------------------------------------------------------
FROM ${RUST_IMAGE} AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY --from=web /src/web/dist /src/web-dist
ENV AMAIL_WEB_DIST=/src/web-dist
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p amail-cli \
    && cp target/release/amail /usr/local/bin/amail

# --- runtime -------------------------------------------------------------------------
FROM ${RUNTIME_IMAGE}
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /data --shell /usr/sbin/nologin amail \
    && mkdir -p /data && chown amail:amail /data
COPY --from=build /usr/local/bin/amail /usr/local/bin/amail
COPY LICENSE THIRD_PARTY_LICENSES.md /usr/share/doc/amail/

USER amail
WORKDIR /data
VOLUME /data
EXPOSE 8025

# Settings come from the environment (see `amail serve --help`); the
# defaults run a single SQLite instance on the volume.
ENV AMAIL_DB=sqlite:///data/amail.db \
    AMAIL_LISTEN=0.0.0.0:8025

HEALTHCHECK --interval=10s --timeout=5s --start-period=10s --retries=3 \
    CMD ["amail", "healthcheck", "http://127.0.0.1:8025"]

ENTRYPOINT ["amail"]
CMD ["serve"]
