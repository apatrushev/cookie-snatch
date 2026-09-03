set dotenv-load

import? 'local.just'

# format, clippy and tests
default: fmt clippy test

# testing, filters available
test *FILTER:
    @echo "Running tests with filter: '{{FILTER}}' and args: ${JUST_TEST_ARGS:-}"
    cargo nextest run {{FILTER}} -- ${JUST_TEST_ARGS:-}

# format and clippy
check: fmt clippy

# format
fmt:
    cargo +nightly fmt

# clippy for code and tests
clippy:
    cargo clippy
    cargo clippy --tests

shear:
    cargo +nightly shear --expand --fix
