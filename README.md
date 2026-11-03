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
```

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
