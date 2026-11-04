#!/usr/bin/env bash
# Release build: bumps the version, builds Gestalt.app and the .dmg signed
# with a Developer ID Application certificate, notarized by Apple and stapled
# (so Gatekeeper opens them on any Mac), then commits the bump and tags it.
# A plain `pnpm tauri build` stays unsigned and leaves the version alone.
#
#   scripts/build-signed.sh                  bump patch (0.1.0 -> 0.1.1)
#   scripts/build-signed.sh --minor          bump minor (0.1.3 -> 0.2.0)
#   scripts/build-signed.sh --major          bump major (0.2.0 -> 1.0.0)
#   scripts/build-signed.sh --version 1.2.3  set an exact version
#   scripts/build-signed.sh --no-bump        rebuild the current version (no commit/tag)
#   scripts/build-signed.sh --no-notarize    sign only, no bump (quick local check)
# Other arguments go to `pnpm tauri build`.
#
# A bump needs a clean working tree and a free tag v<version>. The bump is
# committed as "Release v<version>" and tagged v<version> only after
# the build, notarization and checks all pass; on failure it is reverted.
# The version lives only in src-tauri/Cargo.toml (tauri.conf.json has none).
#
# Tauri signs the app (hardened runtime), notarizes and staples it, then signs
# the dmg. This script picks the identity, supplies the notarization
# credentials, and then notarizes and staples the dmg too, which Tauri doesn't.
#
# Signing identity: the only "Developer ID Application" certificate in the
# keychain, or APPLE_SIGNING_IDENTITY if set.
#
# Notarization credentials, either (checked in this order):
#   App Store Connect API key (recommended):
#     APPLE_API_KEY      key ID
#     APPLE_API_ISSUER   issuer ID
#     APPLE_API_KEY_PATH path to AuthKey_<id>.p8 (optional if the file is in
#                        ~/.appstoreconnect/private_keys or ~/private_keys)
#   Apple ID:
#     APPLE_ID           Apple ID email
#     APPLE_TEAM_ID      team ID
#     APPLE_PASSWORD     app-specific password; if unset it is read from the
#                        login keychain item "gestalt-notarization" for
#                        APPLE_ID (see README)
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
bundle_dir="$root/src-tauri/target/release/bundle"
manifest="src-tauri/Cargo.toml"
lockfile="src-tauri/Cargo.lock"

die() { echo "error: $*" >&2; exit 1; }

[[ "$(uname -s)" == Darwin ]] || die "macOS only"

notarize=1
bump=patch
new_version=""
args=()
while (( $# )); do
  case "$1" in
    --no-notarize) notarize=0 ;;
    --patch|--minor|--major) bump="${1#--}" ;;
    --no-bump) bump=none ;;
    --version)
      [[ $# -ge 2 ]] || die "--version needs a value"
      bump=exact new_version="$2"; shift ;;
    --version=*) bump=exact new_version="${1#--version=}" ;;
    *) args+=("$1") ;;
  esac
  shift
done
(( notarize )) || bump=none # an unnotarized build is never a release

cd "$root"

# --- version ----------------------------------------------------------------
# First `version = "..."` in the manifest is the [package] one.
current_version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$manifest" | head -1)"
[[ "$current_version" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]] \
  || die "can't parse version \"$current_version\" in $manifest"
major="${BASH_REMATCH[1]}" minor="${BASH_REMATCH[2]}" patch="${BASH_REMATCH[3]}"
case "$bump" in
  patch) new_version="$major.$minor.$((patch + 1))" ;;
  minor) new_version="$major.$((minor + 1)).0" ;;
  major) new_version="$((major + 1)).0.0" ;;
  exact) [[ "$new_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "--version must be X.Y.Z, got \"$new_version\"" ;;
  none)  new_version="$current_version" ;;
esac

if [[ "$bump" != none ]]; then
  [[ -z "$(git status --porcelain)" ]] || die "working tree not clean; commit or stash first (or use --no-bump)"
  [[ "$new_version" != "$current_version" ]] || die "version is already $new_version"
  ! git rev-parse -q --verify "refs/tags/v$new_version" >/dev/null || die "tag v$new_version already exists"
  echo "Version: $current_version -> $new_version"
else
  echo "Version: $current_version (not bumped)"
fi

# --- signing identity -------------------------------------------------------
if [[ -z "${APPLE_SIGNING_IDENTITY:-}" ]]; then
  identities="$(security find-identity -v -p codesigning \
    | sed -n 's/.*"\(Developer ID Application: .*\)"$/\1/p' | sort -u)"
  count="$(printf '%s' "$identities" | grep -c . || true)"
  case "$count" in
    0) die "no \"Developer ID Application\" certificate in the keychain (see README → Code signing)" ;;
    1) APPLE_SIGNING_IDENTITY="$identities" ;;
    *) die "several Developer ID Application certificates; set APPLE_SIGNING_IDENTITY to one of:
$identities" ;;
  esac
fi
export APPLE_SIGNING_IDENTITY
echo "Signing identity: $APPLE_SIGNING_IDENTITY"

# --- notarization credentials -----------------------------------------------
notary_auth=()
if (( notarize )); then
  if [[ -n "${APPLE_API_KEY:-}" && -n "${APPLE_API_ISSUER:-}" ]]; then
    if [[ -z "${APPLE_API_KEY_PATH:-}" ]]; then
      for d in "$root/private_keys" "$HOME/private_keys" "$HOME/.private_keys" "$HOME/.appstoreconnect/private_keys"; do
        if [[ -f "$d/AuthKey_$APPLE_API_KEY.p8" ]]; then
          APPLE_API_KEY_PATH="$d/AuthKey_$APPLE_API_KEY.p8"
          break
        fi
      done
      [[ -n "${APPLE_API_KEY_PATH:-}" ]] || die "AuthKey_$APPLE_API_KEY.p8 not found; set APPLE_API_KEY_PATH"
    fi
    export APPLE_API_KEY APPLE_API_ISSUER APPLE_API_KEY_PATH
    unset APPLE_ID APPLE_PASSWORD # Tauri prefers Apple ID creds if both are set
    notary_auth=(--key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY" --issuer "$APPLE_API_ISSUER")
    echo "Notarizing with API key $APPLE_API_KEY"
  elif [[ -n "${APPLE_ID:-}" ]]; then
    [[ -n "${APPLE_TEAM_ID:-}" ]] || die "APPLE_TEAM_ID is required with APPLE_ID"
    if [[ -z "${APPLE_PASSWORD:-}" ]]; then
      APPLE_PASSWORD="$(security find-generic-password -s gestalt-notarization -a "$APPLE_ID" -w 2>/dev/null)" \
        || die "APPLE_PASSWORD not set and no keychain item \"gestalt-notarization\" for $APPLE_ID"
    fi
    export APPLE_ID APPLE_TEAM_ID APPLE_PASSWORD
    notary_auth=(--apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_PASSWORD")
    echo "Notarizing with Apple ID $APPLE_ID"
  else
    die "no notarization credentials (set APPLE_API_KEY + APPLE_API_ISSUER, or APPLE_ID + APPLE_TEAM_ID), or pass --no-notarize"
  fi
else
  # Make sure Tauri doesn't pick up credentials from the environment.
  unset APPLE_ID APPLE_PASSWORD APPLE_API_KEY APPLE_API_ISSUER APPLE_API_KEY_PATH
  echo "Skipping notarization"
fi

# --- bump -------------------------------------------------------------------
if [[ "$bump" != none ]]; then
  # Undo the bump unless everything below succeeds.
  trap 'echo "Build failed; reverting version bump" >&2; git checkout -- "$manifest" "$lockfile"' EXIT
  awk -v old="version = \"$current_version\"" -v new="version = \"$new_version\"" \
    '!done && $0 == old { $0 = new; done = 1 } { print }' "$manifest" > "$manifest.tmp"
  mv "$manifest.tmp" "$manifest"
  grep -q "^version = \"$new_version\"\$" "$manifest" || die "failed to update $manifest"
fi

# --- build ------------------------------------------------------------------
# Remove old bundles so the globs below only see this build's output.
rm -rf "$bundle_dir/macos" "$bundle_dir/dmg"
pnpm tauri build --bundles app,dmg ${args[@]+"${args[@]}"} # also updates Cargo.lock

app="$(ls -d "$bundle_dir"/macos/*.app | head -1)"
dmg="$(ls "$bundle_dir"/dmg/*.dmg | head -1)"

if (( notarize )); then
  echo "Notarizing $(basename "$dmg") (usually a few minutes)…"
  xcrun notarytool submit "$dmg" "${notary_auth[@]}" --wait
  xcrun stapler staple "$dmg"
fi

# --- verify -----------------------------------------------------------------
echo "Verifying…"
built_version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist")"
[[ "$built_version" == "$new_version" ]] || die "app version is $built_version, expected $new_version"
codesign --verify --deep --strict "$app"
codesign --verify --strict "$dmg"
if (( notarize )); then
  xcrun stapler validate "$app"
  xcrun stapler validate "$dmg"
  spctl --assess --type execute "$app"
  spctl --assess --type open --context context:primary-signature "$dmg"
fi
codesign -dv "$app" 2>&1 | grep -E '^(Authority|TeamIdentifier|Runtime Version|Timestamp)='

# --- commit + tag -----------------------------------------------------------
if [[ "$bump" != none ]]; then
  trap - EXIT
  git add "$manifest" "$lockfile"
  git commit -q -m "Release v$new_version"
  git tag -a "v$new_version" -m "Gestalt $new_version"
  echo "Committed and tagged v$new_version (not pushed)"
fi

echo
echo "Version: $new_version"
echo "App: $app"
echo "DMG: $dmg"
