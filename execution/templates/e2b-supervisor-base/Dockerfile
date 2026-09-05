FROM e2bdev/base:latest

USER root

ARG EXECUTION_SUPERVISOR_URL=https://downloads.acentric.dev/execution-supervisor/0.1.0-dev-cac77400/x86_64-unknown-linux-musl/execution-supervisor
ARG EXECUTION_SUPERVISOR_SHA256=eaf4c9a9ab57e7ccf29c0fa30cab47d7fb34b182f9de6e435b40ceb5b56e9ab5

RUN curl --fail --location --silent --show-error \
        "$EXECUTION_SUPERVISOR_URL" \
        --output /tmp/execution-supervisor \
    && echo "$EXECUTION_SUPERVISOR_SHA256  /tmp/execution-supervisor" | sha256sum --check --strict \
    && install -m 0755 /tmp/execution-supervisor /usr/local/bin/execution-supervisor \
    && rm /tmp/execution-supervisor \
    && /usr/local/bin/execution-supervisor version

USER user
