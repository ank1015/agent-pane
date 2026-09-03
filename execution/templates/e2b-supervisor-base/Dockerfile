FROM e2bdev/base:latest

USER root

ARG EXECUTION_SUPERVISOR_URL=https://downloads.acentric.dev/execution-supervisor/0.1.0-dev-c3f64930/x86_64-unknown-linux-musl/execution-supervisor
ARG EXECUTION_SUPERVISOR_SHA256=c3f64930a6a2888b0256e73d4815df01e5fdd46ac25cf37b5ba49591574de0c5

RUN curl --fail --location --silent --show-error \
        "$EXECUTION_SUPERVISOR_URL" \
        --output /tmp/execution-supervisor \
    && echo "$EXECUTION_SUPERVISOR_SHA256  /tmp/execution-supervisor" | sha256sum --check --strict \
    && install -m 0755 /tmp/execution-supervisor /usr/local/bin/execution-supervisor \
    && rm /tmp/execution-supervisor \
    && /usr/local/bin/execution-supervisor version

USER user
