#!/usr/bin/env bash
set -euo pipefail

# Automate a kempt release.
# Usage:
#   ./release.sh <version>   (e.g. ./release.sh 0.2.0)
#   ./release.sh --patch
#   ./release.sh --minor
#   ./release.sh --major

usage() {
  cat <<'EOF'
Usage:
  ./release.sh <version>   (e.g. ./release.sh 0.2.0)
  ./release.sh --patch     (bump latest CHANGELOG.md release by one patch)
  ./release.sh --minor     (bump latest CHANGELOG.md release by one minor)
  ./release.sh --major     (bump latest CHANGELOG.md release by one major)
EOF
}

latest_changelog_version() {
  awk '
    /^## \[Unreleased\]$/ {
      seen_unreleased = 1
      next
    }
    seen_unreleased && /^## \[[0-9]+\.[0-9]+\.[0-9]+\]$/ {
      version = $0
      sub(/^## \[/, "", version)
      sub(/\]$/, "", version)
      print version
      exit
    }
  ' CHANGELOG.md
}

bump_version() {
  local version="$1"
  local bump="$2"
  local major minor patch
  IFS=. read -r major minor patch <<<"$version"

  if [[ ! "$major" =~ ^[0-9]+$ || ! "$minor" =~ ^[0-9]+$ || ! "$patch" =~ ^[0-9]+$ ]]; then
    echo "Error: latest CHANGELOG.md release is not a stable semver version: ${version}"
    exit 1
  fi

  case "$bump" in
    patch)
      patch=$((patch + 1))
      ;;
    minor)
      minor=$((minor + 1))
      patch=0
      ;;
    major)
      major=$((major + 1))
      minor=0
      patch=0
      ;;
    *)
      echo "Error: unknown bump type: ${bump}"
      exit 1
      ;;
  esac

  printf "%s.%s.%s\n" "$major" "$minor" "$patch"
}

if [[ "$#" -ne 1 ]]; then
  usage
  exit 1
fi

case "$1" in
  -h | --help)
    usage
    exit 0
    ;;
  --patch | --minor | --major)
    BUMP="${1#--}"
    LAST_VERSION=$(latest_changelog_version)
    if [[ -z "$LAST_VERSION" ]]; then
      echo "Error: could not find the latest release in CHANGELOG.md."
      exit 1
    fi
    VERSION=$(bump_version "$LAST_VERSION" "$BUMP")
    echo "Latest CHANGELOG.md release is ${LAST_VERSION}; ${BUMP} bump is ${VERSION}"
    ;;
  --*)
    echo "Error: unknown option: $1"
    usage
    exit 1
    ;;
  *)
    VERSION="$1"
    ;;
esac

VERSION="${VERSION#v}"
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Error: version must be stable semver, e.g. 0.2.0"
  exit 1
fi

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
  cargo publish --allow-dirty
fi

echo
echo "Done. Released ${TAG}"
echo "  GitHub Release: https://github.com/${REPO}/releases/tag/${TAG}"
echo "  crates.io:      https://crates.io/crates/kempt-fmt/${VERSION}"
