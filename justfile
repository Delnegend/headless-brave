# The one command CI runs, and the one to run before pushing. Ordered so the
# cheapest and most likely failure comes first.

# Format, lint and test. The container image is not built here: a gate that
# builds is a gate that takes ten minutes.
check:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test