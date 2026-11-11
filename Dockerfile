# syntax=docker/dockerfile:1

# The web app: static files the server serves beside the API.
FROM node:24-trixie-slim AS web
WORKDIR /src
RUN corepack enable
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY web/package.json web/
COPY workers/email-inbound/package.json workers/email-inbound/
RUN --mount=type=cache,id=payvy-pnpm,target=/root/.local/share/pnpm/store \
    pnpm install --frozen-lockfile --filter @payvy/web
COPY web ./web
RUN pnpm --dir web build

FROM rust:1.99.0-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# The registry and target/ are BuildKit cache mounts, so a deploy recompiles
# only Payvy's own crates. Tests run locally and in CI, not here.
RUN --mount=type=cache,id=payvy-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=payvy-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=payvy-target,target=/src/target,sharing=locked \
    cargo build --locked --release -p payvy-server --bin payvy \
    && mkdir -p /out && cp target/release/payvy /out/

FROM debian:trixie-slim AS app
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl poppler-utils tesseract-ocr tesseract-ocr-eng \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --user-group --create-home payvy
COPY --from=build /out/payvy /usr/local/bin/payvy
COPY --from=web /src/web/dist /usr/share/payvy/web
USER payvy
ENV PAYVY_BIND=0.0.0.0:7430 PAYVY_WEB_DIR=/usr/share/payvy/web
EXPOSE 7430
HEALTHCHECK --interval=15s --timeout=3s --start-period=30s --retries=3 \
    CMD curl --fail --silent http://127.0.0.1:7430/health || exit 1
# Migrations run at startup. A command, not an entrypoint, so one-off commands run as
# written: kamal app exec "payvy person add --email ... --name ...".
CMD ["payvy", "serve"]
