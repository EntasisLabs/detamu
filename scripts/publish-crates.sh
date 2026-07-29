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

crate_is_published() {
  local crate="$1"
  cargo info "${crate}@${VERSION}" --registry crates-io >/dev/null 2>&1
}

wait_for_registry() {
  local crate="$1"
  local attempts=0
  until crate_is_published "$crate"; do
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
  status)
    for crate in "${CRATES[@]}"; do
      if crate_is_published "$crate"; then
        echo "published ${crate}@${VERSION}"
      else
        echo "pending   ${crate}@${VERSION}"
      fi
    done
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
      if crate_is_published "$crate"; then
        echo "Skipping ${crate}@${VERSION}; already published."
        continue
      fi
      echo "Publishing ${crate}@${VERSION}"
      if ! cargo publish --locked -p "$crate"; then
        # The upload can succeed even if Cargo loses the response. Recheck the
        # exact registry version before treating the command failure as fatal.
        if crate_is_published "$crate"; then
          echo "Skipping ${crate}@${VERSION}; crates.io now reports it published."
          continue
        fi
        echo "Publishing stopped at ${crate}@${VERSION}." >&2
        echo "After resolving the registry error, rerun this command; published versions will be skipped." >&2
        exit 1
      fi
      wait_for_registry "$crate"
    done
    ;;
  *)
    echo "usage: scripts/publish-crates.sh [check|status|publish]" >&2
    exit 2
    ;;
esac
