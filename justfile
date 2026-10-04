@default:
    just --list

# Check formatting, linting, and run tests
check:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test

# Bump Cargo manifest version and update lockfile
bump version:
    sed -i -E '0,/^version = ".*"/s//version = "{{version}}"/' Cargo.toml
    cargo check