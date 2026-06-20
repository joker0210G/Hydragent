# Installing Hydragent

Hydragent ships with a **one-command installer** that mirrors the Ollama and
OpenClaw experience. Paste one line, get a working `hydragent` install.

> **Maintainers / Forks:** all URLs in the install scripts are derived from
> a single `-Repo` parameter / `HYDRAGENT_REPO` env var (default:
> `joker0210G/Hydragent`). To repoint the installer at your own GitHub
> organisation, pass `-Repo your-org/your-repo` (PowerShell) or set
> `HYDRAGENT_REPO=your-org/your-repo` (bash). See the
> [Repointing the installer]((#repointing-the-installer)) section below.

---

## Quick install

### Windows (PowerShell 5.1 or PowerShell 7+)

```powershell
irm https://joker0210G.github.io/Hydragent/install.ps1 | iex
```

> PowerShell alias mapping:
> `irm` = `Invoke-RestMethod` · `iex` = `Invoke-Expression`
> On Windows PowerShell 5.1 you can also use:
> `iwr -useb https://joker0210G.github.io/Hydragent/install.ps1 | iex`

### macOS / Linux

```bash
curl -fsSL https://joker0210G.github.io/Hydragent/install.sh | sh
```

That's the entire install. The script:

1. Detects your OS and architecture.
2. Downloads the latest prebuilt `hydragent` release.
3. If no release is published for your platform, it falls back to building
   from source — installing Rust via `rustup` if needed.
4. Drops the binary at `~/.hydragent/bin/hydragent`.
5. Adds `~/.hydragent/bin` to your user `PATH`.
6. Creates a data directory at `~/.hydragent/data`.
7. Launches the **onboarding wizard** so you can pick a brain provider and
   paste your API key.

When the wizard finishes you will have a working `hydragent` on `PATH`.

---

## Verify the install

```powershell
hydragent --version     # prints version + git hash
hydragent status        # one-shot dashboard
hydragent ps            # no gateways running yet (expected)
hydragent serve         # starts the gateway in the foreground

# Launch the browser Control UI (token-auth; opens http://127.0.0.1:8765/)
Hydragent ui
```

> **Tip:** open a **new** terminal first. The `PATH` change only applies to
> processes spawned after the installer finishes.

The browser **Control UI** (`adapters/control_ui/`) ships with Hydragent
and is started by the launcher above — it bundles the SPA shell, four
themes, seven locales, PWA install, and VAPID-based Web Push. See
[`doc/CONTROL_UI.md`](doc/CONTROL_UI.md) for the full feature reference.

---

## What `irm ... | iex` actually runs

`irm` downloads the raw PowerShell script and `iex` runs it in your current
PowerShell session. You can inspect it first:

```powershell
irm https://joker0210G.github.io/Hydragent/install.ps1 | Out-File install.ps1
Get-Content install.ps1       # read it
.\install.ps1                 # run it
```

Both forms behave identically. Same for `curl ... | sh` — use `curl -fsSL
... > install.sh` first if you want to audit the script.

---

## Flags & environment variables

### `install.ps1` parameters

| Parameter       | Default            | Description                                                         |
| --------------- | ------------------ | ------------------------------------------------------------------- |
| `-Source`       | _off_              | Force a from-source build (downloads rustup if needed).             |
| `-SkipOnboard`  | _off_              | Don't run `hydragent onboard` at the end.                           |
| `-Force`        | _off_              | Overwrite an existing installation.                                 |
| `-Version`      | `latest`           | Pin a release tag (e.g. `v0.7.2`).                                  |
| `-InstallRoot`  | `%USERPROFILE%\.hydragent` | Override the install directory.                            |
| `-Repo`         | `joker0210G/Hydragent` | GitHub `owner/repo` for source fallback / forks.                  |
| `-Quiet`        | _off_              | Suppress the colored banner (useful for CI logs).                   |

### `install.sh` environment variables

| Variable                 | Default            | Description                                            |
| ------------------------ | ------------------ | ------------------------------------------------------ |
| `HYDRAGENT_VERSION`      | `latest`           | Pin a release tag (e.g. `v0.7.2`).                     |
| `HYDRAGENT_INSTALL_ROOT` | `$HOME/.hydragent` | Override the install directory.                        |
| `HYDRAGENT_REPO`         | `joker0210G/Hydragent` | GitHub `owner/repo` for source fallback / forks.     |
| `HYDRAGENT_SOURCE=1`     | _off_              | Force a from-source build.                             |
| `HYDRAGENT_FORCE=1`      | _off_              | Overwrite an existing installation.                    |
| `HYDRAGENT_SKIP_ONBOARD=1` | _off_            | Don't run `hydragent onboard` at the end.              |
| `NO_COLOR=1`             | _off_              | Disable ANSI colors in installer output.               |

---

## Install paths

The installer drops everything under one user-owned directory:

| Platform | Location                                        |
| -------- | ----------------------------------------------- |
| Windows  | `%USERPROFILE%\.hydragent`                      |
| macOS    | `$HOME/.hydragent`                              |
| Linux    | `$HOME/.hydragent`                              |

Layout:

```
~/.hydragent/
├── bin/
│   ├── hydragent            # the binary (or hydragent.exe on Windows)
│   ├── Hydragent.cmd        # Windows launcher (auto-generated)
│   └── install.{ps1,sh}     # the installer (for future re-runs)
├── .env                     # your config (written by onboard) — top-level
├── data/
│   ├── logs/                # gateway + tool logs
│   └── swarm/               # dreaming worker output
└── src/                     # only on source installs (git clone)
```

The installer **never writes outside this directory** and **never requires
admin elevation**. To uninstall, simply `rm -rf ~/.hydragent` and remove
the `~/.hydragent/bin` entry from your `PATH` / shell rc.

> Note: the `.env` file lives at the **top level** of `~/.hydragent/` (not
> inside `data/`). Putting it there keeps secret material next to its
> owning app, and mirrors the layout used by tools like Claude Code and
> aider. See [`crates/hydragent-core/src/paths.rs`](crates/hydragent-core/src/paths.rs).

---

## First-run onboarding

The installer ends by launching `hydragent onboard`. The wizard asks for:

- **Brain provider** — OpenAI, Anthropic, OpenRouter, Ollama, LM Studio,
  Moonshot, Together, Groq, or any OpenAI-compatible endpoint.
- **API key** — optional for local providers (Ollama, LM Studio).
- **Brain model** — the default model for the live agent brain.
- **Storage root** — defaults to the data directory created above.

Output: `~/.hydragent/.env` (top level). The launcher auto-loads this file on
every subsequent invocation.

To re-run the wizard later:

```powershell
hydragent onboard
```

---

## Updating

The installer is idempotent. Re-running it updates an existing install in
place (unless you pass `-Force` / `HYDRAGENT_FORCE=1`):

```powershell
irm https://joker0210G.github.io/Hydragent/install.ps1 | iex
```

```bash
curl -fsSL https://joker0210G.github.io/Hydragent/install.sh | sh
```

This is the recommended upgrade path — it picks up the latest binary and
re-runs the launcher / `PATH` setup steps.

---

## Air-gapped / offline installs

If the host has no internet access, you have two options:

### Option 1 — clone on a connected machine, copy the binary

```bash
# On a connected machine
git clone https://github.com/joker0210G/Hydragent.git
cd Hydragent
cargo build --release -p hydragent-core
# Copy target/release/hydragent.exe (or `hydragent`) to the offline host.
```

Then drop the binary into `~/.hydragent/bin/` on the offline host. The
launcher (`Hydragent.cmd` / `hydragent`) is a 30-line shim — you can copy
it from the repo root.

### Option 2 — vendor the installer

Download `install.ps1` / `install.sh` (and any assets you need) on a
connected machine, transfer them to the offline host, and run:

```powershell
.\install.ps1 -Source -SkipOnboard
```

```bash
sh install.sh                 # honors HYDRAGENT_SOURCE=1 via env
HYDRAGENT_SOURCE=1 sh install.sh
```

The `-Source` mode will fail without internet (it needs rustup + git +
GitHub clone). For fully offline builds, pre-fetch the source tree on a
connected host and copy it to `$HOME/.hydragent/src` on the target.

---

## Troubleshooting

### "running scripts is disabled on this system"

Windows PowerShell blocks unsigned scripts by default. The installer is
self-contained and only ever downloaded over HTTPS, but if you hit the
execution policy:

```powershell
Set-ExecutionPolicy -Scope CurrentUser -ExecutionPolicy RemoteSigned
```

Then re-run `irm https://joker0210G.github.io/Hydragent/install.ps1 | iex`.

### "cargo: command not found" after install

Open a **new** terminal. The installer updates `PATH` for new processes,
not for the current session.

### Installer fails mid-build

If the source fallback is in progress and `cargo build` errors out, the
output is preserved in your terminal. Common causes:

- **No C linker.** Windows builds need MinGW or the MSVC Build Tools.
  Install [rustup-init with the MSVC default](https://rustup.rs/) (this is
  the default on Windows) — no extra toolchain needed.
- **No internet for crates.io.** Set up a vendored registry or use the
  prebuilt binary path instead (drop `-Source`).
- **Low disk.** A clean release build of `hydragent-core` plus its 15
  internal crates needs ~3 GB free under `target/`.

### "Prebuilt release unavailable"

If the installer can't reach `github.com` (proxy, firewall, private
mirror), set `-Repo your-org/your-fork` and `-Version vX.Y.Z` to point at
an alternate release location.

---

## Uninstall

```powershell
# Windows
Remove-Item -Recurse -Force "$env:USERPROFILE\.hydragent"
[Environment]::SetEnvironmentVariable("Path", `
    ([Environment]::GetEnvironmentVariable("Path","User") -replace [regex]::Escape(";$env:USERPROFILE\.hydragent\bin"), ""), `
    "User")
```

```bash
# macOS / Linux
rm -rf "$HOME/.hydragent"
# Then remove the line that exports PATH="$HOME/.hydragent/bin:..."
# from your ~/.zshrc / ~/.bashrc / ~/.profile.
```

Your `.env`, logs, and dreaming output live under the same `~/.hydragent`
tree, so a single `rm -rf` is a complete uninstall.

---

## How the install chain works end-to-end

For a brand-new user to actually be able to run
`irm https://joker0210G.github.io/Hydragent/install.ps1 | iex` and end up with a working
`hydragent`, four pieces have to be in place. Two are in this repo, two
are in your GitHub repo / domain:

| # | Piece                                | Where it lives            | Status                       |
| - | ------------------------------------ | ------------------------- | ---------------------------- |
| 1 | Installer script (PowerShell)         | `install.ps1` (this repo) | ✅ done                      |
| 2 | Installer script (bash)              | `install.sh`  (this repo) | ✅ done                      |
| 3 | Hosted copy of those scripts (HTTPS)  | GitHub Pages              | ✅ wired up (`pages.yml`)    |
| 4 | Prebuilt release zips                | GitHub Releases           | ✅ wired up (`release.yml`)  |

The release workflow ([`.github/workflows/release.yml`](.github/workflows/release.yml))
runs on every `v*` tag push and produces 8 archives for the matrix:

```
hydragent-0.7.3-x86_64-pc-windows-msvc.zip
hydragent-0.7.3-aarch64-pc-windows-msvc.zip
hydragent-0.7.3-x86_64-unknown-linux-gnu.tar.gz
hydragent-0.7.3-aarch64-unknown-linux-gnu.tar.gz
hydragent-0.7.3-x86_64-unknown-linux-musl.tar.gz
hydragent-0.7.3-aarch64-unknown-linux-musl.tar.gz
hydragent-0.7.3-x86_64-apple-darwin.tar.gz
hydragent-0.7.3-aarch64-apple-darwin.tar.gz
```

The names are exactly what [`install.ps1`](install.ps1) / [`install.sh`](install.sh)
look up, so no further configuration is needed once you push a tag.

The pages workflow ([`.github/workflows/pages.yml`](.github/workflows/pages.yml))
publishes `install.ps1`, `install.sh`, and `docs/index.html` to the
`gh-pages` branch on every push to `main`. Once GitHub Pages is enabled
in the repo settings, those files are reachable at:

- `https://joker0210G.github.io/Hydragent/install.ps1`
- `https://joker0210G.github.io/Hydragent/install.sh`
- `https://joker0210G.github.io/Hydragent/` (landing page)

That gives you a **working `*|iex` one-liner with no custom domain**.
If you later point a custom domain (e.g. `hydragent.dev`) at
`joker0210G.github.io` via a CNAME and add `docs/CNAME` containing the
bare domain, the **exact same URLs also work at the custom domain**
(e.g. `https://hydragent.dev/install.ps1`).

### To cut a release

```bash
git tag v0.7.3
git push origin v0.7.3
```

GitHub Actions does the rest. Within ~10 minutes the release appears at
`https://github.com/joker0210G/Hydragent/releases/tag/v0.7.3` with all 8
archives attached.

### To enable GitHub Pages

In the GitHub web UI: **Settings → Pages → Build and deployment → Source:
Deploy from a branch → Branch: `gh-pages` / `/ (root)`**. The pages
workflow takes care of populating the `gh-pages` branch — you just have
to switch Pages on once.

### To set up a custom domain (optional, future)

1. Buy a domain (e.g. `hydragent.dev`) from any registrar.
2. Add a CNAME record: `<your-domain> -> joker0210G.github.io.`
3. Create `docs/CNAME` in this repo containing the bare domain
   (e.g. `hydragent.dev`). The pages workflow will publish it.
4. In GitHub Pages settings, enter the custom domain and enable HTTPS
   (Let's Encrypt cert is automatic).

---

## Repointing the installer (for forks / private mirrors)

All URLs in the install scripts are derived from a single `-Repo`
parameter / `HYDRAGENT_REPO` env var. Default: `joker0210G/Hydragent`.

To repoint for your fork:

```powershell
irm https://joker0210G.github.io/Hydragent/install.ps1 | iex -Repo myorg/my-hydragent
```

```bash
HYDRAGENT_REPO=myorg/my-hydragent \
    curl -fsSL https://joker0210G.github.io/Hydragent/install.sh | sh
```

This single parameter drives:

- the hosted install script URL
- the GitHub Releases URL (for prebuilt downloads)
- the source clone URL (for the `-Source` fallback)

If you're hosting a permanent fork and want the URL in the scripts
themselves to point at your org, do a single find-and-replace of the
literal `joker0210G` (the GitHub org) in:

- `install.ps1`   (1 occurrence — the `-Repo` default)
- `install.sh`    (1 occurrence — the `REPO=` default)
- `Hydragent.cmd` (2 occurrences — the `:do_install` download URLs)
- `docs/index.html` (1 occurrence — the GitHub link in the footer)

That's it. No other URL is hard-coded in the install scripts.

---

## See also

- **[ONBOARDING.md](ONBOARDING.md)** — the 4-step developer workflow (clone +
  build + run), for contributors working from a checkout.
- **[README.md](README.md)** — overview, features, CLI reference.
- **[doc/ARCHITECTURE.md](doc/ARCHITECTURE.md)** — how the 16-crate runtime
  fits together.
