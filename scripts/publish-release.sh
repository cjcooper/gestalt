#!/usr/bin/env bash
# Publishes the dmg made by scripts/build-signed.sh as a GitHub release, which
# is what the Homebrew cask (cjcooper/homebrew-tap, Casks/gestalt.rb) installs:
#
#   https://github.com/cjcooper/gestalt/releases/download/v<version>/Gestalt_<version>_aarch64.dmg
#
#   scripts/publish-release.sh            publish the version in src-tauri/Cargo.toml
#   scripts/publish-release.sh --dry-run  run every check, push and upload nothing
#   scripts/publish-release.sh --clobber  replace the assets of an existing release
#
# Run on main, right after `scripts/build-signed.sh` (which commits and tags
# v<version> but doesn't push). Before uploading it checks that:
#   - the branch is main, the tree is clean, v<version> is in its history,
#     and nothing built into the app changed since (only scripts/, README.md
#     or .github/ may, so fixing these scripts after a build is fine);
#   - the dmg is Gestalt_<version>_aarch64.dmg, is signed, notarized and
#     stapled, and holds Gestalt.app at version <version> for arm64;
#   - the .sha256 next to it matches the dmg.
# Then it pushes main and the tag, creates the release marked "latest" (the
# cask's livecheck uses :github_latest), downloads the asset again to verify
# its SHA-256, and prints the values for the cask.
set -euo pipefail

repo="cjcooper/gestalt"
tap_cask="cjcooper/tap/gestalt"
root="$(cd "$(dirname "$0")/.." && pwd)"
manifest="src-tauri/Cargo.toml"

die() { echo "error: $*" >&2; exit 1; }

dry_run=0
clobber=0
while (( $# )); do
  case "$1" in
    --dry-run) dry_run=1 ;;
    --clobber) clobber=1 ;;
    -h|--help) sed -n '2,21p' "$0"; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
  shift
done

[[ "$(uname -s)" == Darwin ]] || die "macOS only"
command -v gh >/dev/null || die "gh (GitHub CLI) not found: brew install gh"
gh auth status >/dev/null 2>&1 || die "gh isn't logged in: gh auth login"

cd "$root"

version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$manifest" | head -1)"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "can't parse version \"$version\" in $manifest"
tag="v$version"
name="Gestalt_${version}_aarch64.dmg"
dmg="$root/src-tauri/target/release/bundle/dmg/$name"
sha_file="$dmg.sha256"
echo "Version: $version ($tag)"

# --- git --------------------------------------------------------------------
branch="$(git symbolic-ref --short HEAD 2>/dev/null || true)"
[[ "$branch" == main ]] || die "on \"$branch\"; publish from main"
[[ -z "$(git status --porcelain)" ]] || die "working tree not clean"
git rev-parse -q --verify "refs/tags/$tag" >/dev/null \
  || die "no tag $tag; run scripts/build-signed.sh first"
tag_commit="$(git rev-parse "$tag^{commit}")"
git merge-base --is-ancestor "$tag_commit" HEAD || die "$tag isn't in main's history"
# The dmg was built at the tag; anything that goes into the app must be unchanged.
changed="$(git diff --name-only "$tag_commit" HEAD -- . ':!scripts' ':!README.md' ':!.github')"
[[ -z "$changed" ]] || die "changed since $tag, so the dmg is stale; release a new version:
$changed"
git fetch -q origin main --tags
git merge-base --is-ancestor origin/main HEAD \
  || die "origin/main has commits that local main doesn't; pull (and rebuild) first"
# An annotated tag lists as "<tag obj> refs/tags/vX" plus "<commit> refs/tags/vX^{}".
remote_tag="$(git ls-remote --tags origin \
  | awk -v t="refs/tags/$tag^{}" -v l="refs/tags/$tag" '$2 == t { p = $1 } $2 == l { r = $1 } END { print (p ? p : r) }')"
if [[ -n "$remote_tag" && "$remote_tag" != "$tag_commit" ]]; then
  die "origin's $tag points at a different commit"
fi

# --- dmg --------------------------------------------------------------------
[[ -f "$dmg" ]] || die "missing $dmg; run scripts/build-signed.sh"
[[ -f "$sha_file" ]] || die "missing $sha_file; run scripts/build-signed.sh"
sha256="$(shasum -a 256 "$dmg" | cut -d' ' -f1)"
[[ "$(cut -d' ' -f1 "$sha_file")" == "$sha256" ]] \
  || die "$name doesn't match its .sha256 (rebuilt or re-stapled since?)"

echo "Checking ${name}…"
codesign --verify --strict "$dmg"
xcrun stapler validate -q "$dmg" || die "$name isn't notarized and stapled"
spctl --assess --type open --context context:primary-signature "$dmg" \
  || die "Gatekeeper rejects $name"

mnt="$(mktemp -d)"
trap 'hdiutil detach -quiet "$mnt" 2>/dev/null || true; rmdir "$mnt" 2>/dev/null || true' EXIT
hdiutil attach -quiet -readonly -nobrowse -noautoopen -mountpoint "$mnt" "$dmg"
app="$mnt/Gestalt.app"
[[ -d "$app" ]] || die "$name has no Gestalt.app (the cask installs app \"Gestalt.app\")"
plist="$app/Contents/Info.plist"
app_version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$plist")"
bundle_id="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$plist")"
exe="$app/Contents/MacOS/$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$plist")"
[[ "$app_version" == "$version" ]] || die "Gestalt.app in the dmg is $app_version, expected $version"
[[ "$bundle_id" == com.it-problems.gestalt ]] || die "bundle id is $bundle_id, expected com.it-problems.gestalt"
lipo -archs "$exe" | grep -qw arm64 || die "$exe isn't arm64 ($(lipo -archs "$exe"))"
xcrun stapler validate -q "$app" || die "Gestalt.app in the dmg isn't stapled"
spctl --assess --type execute "$app" || die "Gatekeeper rejects Gestalt.app"
hdiutil detach -quiet "$mnt"
trap - EXIT
rmdir "$mnt"
echo "OK: Gestalt.app $app_version ($bundle_id, arm64), notarized"
echo "SHA-256: $sha256"

# --- release ----------------------------------------------------------------
release_exists=0
gh release view "$tag" -R "$repo" >/dev/null 2>&1 && release_exists=1
(( release_exists && ! clobber )) && die "release $tag already exists; pass --clobber to replace its assets"

if (( dry_run )); then
  echo
  echo "Dry run: would push main and $tag to origin, then"
  (( release_exists )) && echo "  replace the assets of release $tag" \
                       || echo "  create release $tag (latest) with $name and $name.sha256"
  exit 0
fi

git push origin main "refs/tags/$tag"

if (( release_exists )); then
  gh release upload "$tag" "$dmg" "$sha_file" -R "$repo" --clobber
else
  gh release create "$tag" "$dmg" "$sha_file" -R "$repo" \
    --verify-tag --latest --title "Gestalt $version" --generate-notes
fi

# --- verify the upload ------------------------------------------------------
echo "Verifying the download…"
url="https://github.com/$repo/releases/download/$tag/$name"
remote_sha="$(curl -fsSL "$url" | shasum -a 256 | cut -d' ' -f1)"
[[ "$remote_sha" == "$sha256" ]] || die "downloaded $url has SHA-256 $remote_sha, expected $sha256"
latest="$(gh release view -R "$repo" --json tagName -q .tagName)"
[[ "$latest" == "$tag" ]] || echo "warning: GitHub's latest release is $latest, not $tag (livecheck uses latest)" >&2

echo
echo "Published $url"
echo
echo "Cask (Casks/gestalt.rb):"
echo "  version \"$version\""
echo "  sha256 \"$sha256\""
echo
echo "Or: brew bump-cask-pr --version $version --sha256 $sha256 $tap_cask"
