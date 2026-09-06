FROM e2bdev/base:latest

USER root

ARG EXECUTION_SUPERVISOR_URL=https://downloads.acentric.dev/execution-supervisor/0.1.0-dev-7d4c8ad2/x86_64-unknown-linux-musl/execution-supervisor
ARG EXECUTION_SUPERVISOR_SHA256=cc8f6aa9aefaabcbc3a40e22202bc2715d833a5b5b046625706b12fc62e38891

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ripgrep \
    && rm -rf /var/lib/apt/lists/* \
    && curl --fail --location --silent --show-error \
        "$EXECUTION_SUPERVISOR_URL" \
        --output /tmp/execution-supervisor \
    && echo "$EXECUTION_SUPERVISOR_SHA256  /tmp/execution-supervisor" | sha256sum --check --strict \
    && install -m 0755 /tmp/execution-supervisor /usr/local/bin/execution-supervisor \
    && rm /tmp/execution-supervisor \
    && /usr/local/bin/execution-supervisor version

USER user
