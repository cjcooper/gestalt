# Gestalt

Desktop app (Tauri 2 + TypeScript) for running OpenTofu across a folder tree.
Right-click a folder to `tofu plan`, `tofu apply…` (always from a reviewed,
saved plan, with init, plan and apply all in one output pane) or `tofu init`.

To free disk space, right-click any folder and choose **Clean .terraform
Caches…** to remove every `.terraform` folder and `.terraform.lock.hcl` in it
and below, or use **Workspace › Clean All .terraform Caches…** for the whole
opened folder. The next plan re-runs `tofu init` to download them again.

## Development

```sh
pnpm install
pnpm tauri dev
cd src-tauri && cargo test
pnpm tauri build        # .app / .dmg in src-tauri/target/release/bundle
```

### Code signing (macOS)

`pnpm tauri build` makes an unsigned app, which Gatekeeper blocks on other
Macs, and leaves the version alone. For a release to hand out, run:

```sh
scripts/build-signed.sh                  # bump patch, sign, notarize, commit + tag
scripts/build-signed.sh --minor          # or --major, or --version 1.2.3
scripts/build-signed.sh --no-bump        # rebuild the current version, no commit
scripts/build-signed.sh --no-notarize    # sign only, no bump (quick local check)
```

It signs with the hardened runtime using your Developer ID Application
certificate, has Apple notarize the app and the dmg, staples the tickets, and
checks the result with `codesign`, `stapler` and `spctl`. Notarizing takes a
few minutes per submission (two: app, then dmg).

The version lives only in `src-tauri/Cargo.toml` (`tauri.conf.json` has none,
so Tauri reads it from there). A bump needs a clean working tree; once
everything passes the script commits `Release v<version>` and tags
`v<version>` (not pushed). If anything fails, the bump is reverted.

Never export `APPLE_SIGNING_IDENTITY` in your shell profile: `pnpm tauri
build` signs whenever it is set.

One-time setup:

1. **Certificate.** In Xcode → Settings → Accounts → (team) → Manage
   Certificates, add a **Developer ID Application** certificate (or create it
   at developer.apple.com → Certificates and import it). Only the Account
   Holder can create one. Check it's in the keychain:
   `security find-identity -v -p codesigning`. With more than one, set
   `APPLE_SIGNING_IDENTITY` to the full name.
2. **Notarization credentials**, either:
   - **API key** (recommended): App Store Connect → Users and Access →
     Integrations → App Store Connect API → Team Keys, create a key with the
     Developer role. Save `AuthKey_<KEY_ID>.p8` to
     `~/.appstoreconnect/private_keys/` and set:
     ```sh
     export APPLE_API_KEY=<KEY_ID> APPLE_API_ISSUER=<ISSUER_ID>
     ```
   - **Apple ID**: create an app-specific password at account.apple.com →
     Sign-In and Security, store it in the login keychain, and set:
     ```sh
     security add-generic-password -s gestalt-notarization -a <apple-id> -w
     export APPLE_ID=<apple-id> APPLE_TEAM_ID=<TEAM_ID>
     ```

Making the dmg drives Finder with AppleScript; the first run asks to let your
terminal control Finder. If it fails with "Not authorized to send Apple events
to Finder", allow it in System Settings → Privacy & Security → Automation.

Never commit the `.p8` key or the password (`*.p8` and `private_keys/` are
git-ignored).

## Startup folder

Gestalt opens the first of:

1. A directory passed on the command line: `gestalt /path/to/infra`
2. `GESTALT_ROOT`, from the app's environment or your login shell
   (e.g. `export GESTALT_ROOT=~/src/infra` in `~/.zshrc`)
3. The folder you opened last

For `pnpm tauri dev`, set it in the gitignored `src-tauri/.cargo/config.toml`:

```toml
[env]
GESTALT_ROOT = "/path/to/infra"
```

## Environment

tofu runs with, lowest to highest precedence: the app's environment, your
login shell's environment, then every `.gestalt.env` from the opened folder
down to the module being run (deeper files override). See
`src-tauri/src/env_file.rs` for the file format.

## License

Gestalt is released under the [MIT License](LICENSE), Copyright (c) 2026 it-problems.

### Third-party licenses

Gestalt ships many open-source packages. Their licenses are listed in the
app under **About Gestalt** and in [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md).

After adding, removing or upgrading any Rust or npm dependency, regenerate
both (needs `cargo install cargo-about`) and commit the results:

```sh
node scripts/licenses.mjs          # writes THIRD_PARTY_NOTICES.md and public/third-party-licenses.json
node scripts/licenses.mjs --check  # fails if they're stale
```

A new license kind must be added to `about.toml` on purpose. Adapted content
that isn't a package (e.g. the Lucide icon shapes in `src/icons.ts`) is
credited in `scripts/licenses.mjs`.
