# Gestalt

Desktop app (Tauri 2 + TypeScript) for running OpenTofu across a folder tree.
Right-click a folder to `tofu plan`, `tofu apply…` (always from a reviewed,
saved plan, with init, plan and apply all in one output pane) or `tofu init`.

To free disk space, right-click any folder and choose **Clean .terraform
Caches…** to remove every `.terraform` folder and `.terraform.lock.hcl` in it
and below, or use **Workspace › Clean All .terraform Caches…** for the whole
opened folder. The next plan re-runs `tofu init` to download them again.

Right-click a file or folder and choose **Move "…" to Trash…** to delete it
(after a confirmation; it goes to the system Trash, so it can be restored).
A symlink is removed as a link: what it points to stays.

Double-click a file in the tree to edit it in its own window, with syntax
highlighting (HCL for `.tf`/`.tfvars`, plus JSON, YAML, Markdown, shell and
more). It's a normal window, so you can switch back to the tree and plan
while it stays open. ⌘S saves (or the Save button in the bar at the top) and
⌘W closes. By default changes also save when you close the window or click
away from it, e.g. back to the folder's window to run a plan; turn that off
under **Settings… › Save automatically** (⌘,) and closing with unsaved
changes asks first. A save is refused if the file changed on disk since you
opened it, unless you choose to overwrite. For a file in a module folder
(one with a `main.tf`), **Plan** in the editor's bar saves the file and plans
in the window it was opened from, which comes forward to show the output.

Symlinks show a link icon in the tree. A linked file (say a shared
`providers.tf` linked into many modules) opens with a warning bar naming the
real file: changes there affect every place that links to it. It still
belongs to the folder you opened it from, so Plan runs there.
Closing a window closes the editor windows opened from it first.

## Development

```sh
pnpm install
pnpm tauri dev
pnpm test               # frontend (Vitest)
pnpm test:rust          # backend (cargo test)
pnpm tauri build        # .app / .dmg in src-tauri/target/release/bundle
```

### Coverage

Both halves are held at 100%:

```sh
pnpm coverage           # frontend: lines, branches, functions, statements
pnpm coverage:rust      # backend: lines and functions (needs cargo-llvm-cov)
```

The Rust check leaves out only code with nothing to assert on, kept in its own
files for that reason: `main.rs`, `app.rs` (the Tauri builder),
`system_colors/mac.rs` (AppKit calls), `os_trash.rs` (the system Trash call;
what may be deleted is checked in `delete.rs`) and `src/probes/` (manual `#[ignore]`
checks against real repos). New logic belongs elsewhere, under test.

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

The release dmg is
`src-tauri/target/release/bundle/dmg/Gestalt_<version>_aarch64.dmg` (Apple
Silicon only; the script refuses to run on an Intel Mac), with
`Gestalt_<version>_aarch64.dmg.sha256` next to it, hashed after stapling.

### Publishing (GitHub release + Homebrew)

The Homebrew cask (`Casks/gestalt.rb` in `cjcooper/homebrew-tap`) installs
`https://github.com/cjcooper/gestalt/releases/download/v<version>/Gestalt_<version>_aarch64.dmg`.
To publish, on `main`:

```sh
scripts/build-signed.sh                  # bump, sign, notarize, commit + tag
scripts/publish-release.sh --dry-run     # optional: every check, nothing pushed
scripts/publish-release.sh               # push main + tag, create the GitHub release
```

`publish-release.sh` needs `gh` logged in. It checks that a clean `main` has
the tag `v<version>` in its history, with nothing built into the app changed
since (only `scripts/`, `README.md` and `.github/` may change); that the dmg
has the right name, matches its `.sha256`, and is notarized and stapled; and
that it holds an arm64 `Gestalt.app` at that version. Then it pushes, creates
the release marked latest (the cask's livecheck follows the latest release),
downloads the asset again to verify its SHA-256, and prints the `version` /
`sha256` for the cask, along with a `brew bump-cask-pr` command. `--clobber`
replaces the assets of a release that already exists. A changed dmg means a
new `sha256` in the cask.

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

If `pnpm tauri build` fails with "Failed to create app Assets.car … Bad file
descriptor": Tauri runs `actool` with stdin closed, which makes it fail and
leaves Xcode's `ibtoold` daemon broken, so later `actool` runs fail too.
`build-signed.sh` works around it with an `actool` wrapper that supplies
`/dev/null` as stdin. For a plain build, use the wrapper the script leaves in
`src-tauri/target/actool-shim` and restart the daemon:

```sh
killall ibtoold; PATH="$PWD/src-tauri/target/actool-shim:$PATH" pnpm tauri build
```

Never commit the `.p8` key or the password (`*.p8` and `private_keys/` are
git-ignored).

## Opening folders from a terminal

```sh
gestalt ~/src/infra            # open a folder in its own window
gestalt infra/accounts other   # several at once; no argument opens the current folder
```

Each folder gets its own window, with its own runs. If Gestalt is already
running, the folders open as new windows there; a folder that is already open
brings its window forward. **Workspace › New Window** (⇧⌘N) opens an empty one.

The `gestalt` command ships inside the app. Put it on your PATH once:

```sh
ln -s /Applications/Gestalt.app/Contents/Resources/cli/gestalt-cli /usr/local/bin/gestalt
```

(The Homebrew cask can do this with
`binary "#{appdir}/Gestalt.app/Contents/Resources/cli/gestalt-cli", target: "gestalt"`.)

Launched without a folder, the first window opens the first of:

1. `GESTALT_ROOT`, from the app's environment or your login shell
   (e.g. `export GESTALT_ROOT=~/src/infra` in `~/.zshrc`)
2. The folder you opened last

For `pnpm tauri dev`, set it in the gitignored `src-tauri/.cargo/config.toml`:

```toml
[env]
GESTALT_ROOT = "/path/to/infra"
```

## Cloud settings

Each folder can name the cloud account tofu uses there and in every folder
below it: an AWS profile and region, a gcloud configuration and project, an
Azure subscription and tenant. Click the cloud button in the toolbar (the open
folder), right-click a folder, or use **Workspace › Cloud Settings…**. The
choices come from `~/.aws/config` and `~/.aws/credentials`, `gcloud config
configurations` and `az login`. Each field is inherited from the nearest
folder above that sets it, and folders with settings of their own are marked
in the tree.

Settings are kept per computer in Gestalt's config folder
(`~/Library/Application Support/com.it-problems.gestalt/cloud.json` on macOS),
keyed by folder path, never in the repo, so everyone can use their own profile
names.

tofu runs with your login shell's environment plus the folder's cloud
settings, which set:

| Setting | Variables |
| --- | --- |
| AWS profile | `AWS_PROFILE` (and removes `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN` inherited from the shell, which would win over it) |
| AWS region | `AWS_REGION`, `AWS_DEFAULT_REGION` |
| gcloud configuration | `CLOUDSDK_ACTIVE_CONFIG_NAME` |
| Google Cloud project | `GOOGLE_PROJECT`, `CLOUDSDK_CORE_PROJECT` (default: the configuration's project) |
| Azure subscription | `ARM_SUBSCRIPTION_ID` |
| Azure tenant | `ARM_TENANT_ID` (default: the subscription's tenant) |

Each run's output starts with the settings it used and the folder each came
from.

`.gestalt.env` files are no longer read. A run in a folder that still has one
(in it or above it) stops with an error rather than quietly ignoring an
`AWS_PROFILE` you relied on: set the account in Cloud Settings and delete the
file.

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
