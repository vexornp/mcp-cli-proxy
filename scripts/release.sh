#!/usr/bin/env bash
# Bump version, tag, and push to trigger the release workflow.
#
# Usage:
#   ./scripts/release.sh              # bump patch (0.1.0 -> 0.1.1)
#   ./scripts/release.sh patch        # bump patch
#   ./scripts/release.sh minor        # bump minor (0.1.0 -> 0.2.0)
#   ./scripts/release.sh major        # bump major (0.1.0 -> 1.0.0)
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

BUMP="${1:-patch}"
case "$BUMP" in
  patch|minor|major) ;;
  *)
    echo "Usage: $0 [patch|minor|major]" >&2
    exit 1
    ;;
esac

# Ensure main is up to date with remote.
git fetch origin main
LOCAL=$(git rev-parse HEAD)
REMOTE=$(git rev-parse origin/main)
if [ "$LOCAL" != "$REMOTE" ]; then
  echo "Error: local main is out of sync with origin/main." >&2
  echo "  local:  $LOCAL" >&2
  echo "  remote: $REMOTE" >&2
  echo "  Push or pull first." >&2
  exit 1
fi

# Ensure working tree is clean.
if [ -n "$(git status --porcelain)" ]; then
  echo "Error: working tree has uncommitted changes." >&2
  git status --short >&2
  exit 1
fi

# Read current version from Cargo.toml.
CURRENT=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
IFS='.' read -r MAJOR MINOR PATCH <<< "$CURRENT"

case "$BUMP" in
  major)  MAJOR=$((MAJOR+1)); MINOR=0; PATCH=0 ;;
  minor)  MINOR=$((MINOR+1)); PATCH=0 ;;
  patch)  PATCH=$((PATCH+1)) ;;
esac

NEW_VERSION="${MAJOR}.${MINOR}.${PATCH}"
TAG="v${NEW_VERSION}"

echo "Bumping $CURRENT -> $NEW_VERSION"

# Update Cargo.toml and Cargo.lock.
sed -i '' "s/^version = \"$CURRENT\"/version = \"$NEW_VERSION\"/" Cargo.toml
cargo check -q 2>/dev/null  # refresh Cargo.lock version

git add Cargo.toml Cargo.lock
git commit -m "chore: bump version to $NEW_VERSION"
git tag "$TAG"
git push origin main
git push origin "$TAG"

echo "Pushed $TAG — release workflow triggered."
echo "Watch: gh run watch --repo vexornp/mcp-cli-proxy"
