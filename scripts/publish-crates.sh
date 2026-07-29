#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\([^"]*\)"/\1/p' Cargo.toml)"
MODE="${1:-check}"

CRATES=(
  detamu-core
  detamu-runtime
  detamu-model
  detamu-store
  detamu-model-code
  detamu-language
  detamu-language-lsp
  detamu-language-tree-sitter
  detamu-query
  detamu-sdk
  detamu-source-git
  detamu-surreal
  detamu-code-coverage
  detamu-language-lizard
  detamu-language-rust
  detamu-language-rust-analyzer
  detamu-query-code
  detamu
  detamu-engine
)

check_workspace() {
  cargo fmt --all --check
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo test --workspace --all-features
  cargo package --workspace --no-verify --allow-dirty
}

wait_for_registry() {
  local crate="$1"
  local attempts=0
  until cargo info "${crate}@${VERSION}" --registry crates-io >/dev/null 2>&1; do
    attempts=$((attempts + 1))
    if [[ "$attempts" -ge 30 ]]; then
      echo "Timed out waiting for ${crate}@${VERSION} to reach the crates.io index." >&2
      return 1
    fi
    sleep 5
  done
}

case "$MODE" in
  check)
    check_workspace
    ;;
  publish)
    if [[ "${DETAMU_PUBLISH:-}" != "1" ]]; then
      echo "Refusing to publish. Re-run with DETAMU_PUBLISH=1 after reviewing the release." >&2
      exit 2
    fi
    if [[ -n "$(git status --porcelain)" ]]; then
      echo "Refusing to publish from a dirty worktree." >&2
      exit 2
    fi
    check_workspace
    for crate in "${CRATES[@]}"; do
      echo "Publishing ${crate}@${VERSION}"
      cargo publish --locked -p "$crate"
      wait_for_registry "$crate"
    done
    ;;
  *)
    echo "usage: scripts/publish-crates.sh [check|publish]" >&2
    exit 2
    ;;
esac
