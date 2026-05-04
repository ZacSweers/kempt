#!/usr/bin/env bash
set -euo pipefail

# Automate a kempt release.
# Usage: ./release.sh <version>   (e.g. ./release.sh 0.2.0)

VERSION="${1:-}"

if [[ -z "$VERSION" ]]; then
  echo "Usage: ./release.sh <version>"
  echo "Example: ./release.sh 0.2.0"
  exit 1
fi

VERSION="${VERSION#v}"
TAG="v${VERSION}"
REPO="ZacSweers/kempt"

echo "Releasing ${TAG}"
echo "================"
echo

# --- Preflight ---
if ! command -v gh &>/dev/null; then
  echo "Error: 'gh' CLI is required. Install from https://cli.github.com/"
  exit 1
fi

if ! command -v cargo &>/dev/null; then
  echo "Error: 'cargo' is required."
  exit 1
fi

if [[ -n "$(git status --porcelain)" ]]; then
  echo "Error: working tree is dirty. Commit or stash first."
  exit 1
fi

if git rev-parse "$TAG" &>/dev/null; then
  echo "Error: tag ${TAG} already exists."
  exit 1
fi

# --- CHANGELOG.md ---
echo "Updating CHANGELOG.md"
DATE=$(date +%Y-%m-%d)
if ! grep -q "## \[Unreleased\]" CHANGELOG.md; then
  echo "Error: CHANGELOG.md is missing an [Unreleased] section."
  exit 1
fi
sed -i '' "s/## \[Unreleased\]/## [Unreleased]\\
\\
## [${VERSION}]\\
\\
_${DATE}_/" CHANGELOG.md

# --- Bump Cargo.toml ---
echo "Bumping version to ${VERSION}"
sed -i '' "s/^version = \".*\"/version = \"${VERSION}\"/" Cargo.toml

# --- Refresh Cargo.lock ---
echo "Building to refresh Cargo.lock"
cargo build

# --- Commit + push ---
echo "Committing version bump"
git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "Prepare release ${VERSION}"
git push origin main

# --- Tag + push ---
echo "Tagging ${TAG}"
git tag "$TAG"
git push origin "$TAG"

# --- Wait for release workflow ---
echo "Waiting for release workflow to start"
sleep 5

RUN_ID=""
for _ in {1..10}; do
  RUN_ID=$(gh run list --repo "$REPO" --workflow=release.yml --branch="$TAG" --json databaseId,headBranch --jq ".[] | select(.headBranch == \"${TAG}\") | .databaseId" | head -1)
  if [[ -n "$RUN_ID" ]]; then
    break
  fi
  sleep 3
done

if [[ -z "$RUN_ID" ]]; then
  echo "Error: could not find release workflow run for ${TAG}."
  echo "Check https://github.com/${REPO}/actions"
  exit 1
fi

echo "Watching release workflow (run ${RUN_ID})"
gh run watch "$RUN_ID" --repo "$REPO"

STATUS=$(gh run view "$RUN_ID" --repo "$REPO" --json conclusion -q .conclusion)
if [[ "$STATUS" != "success" ]]; then
  echo "Error: release workflow failed (${STATUS})."
  echo "Check https://github.com/${REPO}/actions/runs/${RUN_ID}"
  exit 1
fi

echo "Release workflow succeeded"

# --- crates.io ---
read -rp "Publish to crates.io? [Y/n] " reply
if [[ -z "$reply" || "$reply" =~ ^[Yy] ]]; then
  echo "Publishing to crates.io"
  cargo publish
fi

echo
echo "Done. Released ${TAG}"
echo "  GitHub Release: https://github.com/${REPO}/releases/tag/${TAG}"
echo "  crates.io:      https://crates.io/crates/kempt/${VERSION}"
