# cx internal edition

`cx` internal edition is a Rust TUI launcher for `copilot`, `claude`, and `codex`.
This edition is designed for GitLab-hosted internal distribution:

- provider/model config is embedded at build time
- normal usage is gated by GitLab login
- GitLab CI builds and publishes release assets
- GitLab Release install script is the primary installation path
- npm/npx is provided as a secondary GitLab-based distribution path

## Runtime model

- `cx`: choose agent first, then provider/model
- `cx <agent> [args...]`: skip agent selection, still choose provider/model
- after selection, passthrough args are forwarded unchanged to the native CLI
- for `codex`, `cx` injects a synthetic DashScope provider view before launch, so it does not depend on the user already having `~/.codex/config.toml`
- `cx` does **not** proxy `codex app`
- this internal edition must complete `cx login` before `cx`, `cx <agent> ...`, or `cx probe`

## Login commands

```bash
cx login
cx whoami
cx logout
```

`cx login` uses GitLab OAuth Authorization Code + PKCE against `https://git.huayi.tech`
with callback `http://127.0.0.1:38081/callback`.

## Install from GitLab Release

This is the primary installation path for internal users.

```bash
export CX_GITLAB_TOKEN=<gitlab-personal-access-token>
curl -fsSL \
  -H "PRIVATE-TOKEN: ${CX_GITLAB_TOKEN}" \
  "https://git.huayi.tech/awesome/cx/-/releases/permalink/latest/downloads/install.sh" | sh
```

The script downloads the latest matching release asset, verifies `SHA256SUMS`,
and installs `cx` to `~/.local/bin/cx` by default.

## Install with npx

`npx` is available as a secondary GitLab-based path. Because the project and
release assets are private, users must configure npm registry auth and provide a
GitLab token for the wrapper to download the native binary.

```bash
npm config set @awesome:registry https://git.huayi.tech/api/v4/projects/<project-id>/packages/npm/
npm config set -- //git.huayi.tech/api/v4/projects/<project-id>/packages/npm/:_authToken=<gitlab-token>

export CX_GITLAB_TOKEN=<gitlab-personal-access-token>
npx @awesome/cx login
```

## Build-time embedded config

The internal edition no longer reads `~/.config/cx/config.yaml` at runtime.
Instead, `build.rs` embeds `config/internal.config.yaml` into the binary and
substitutes these CI variables if present:

- `CX_DASHSCOPE_API_KEY`
- `CX_ANTHROPIC_API_KEY`
- `CX_MIMO_API_KEY`

Optional embedded GitLab settings:

- `CX_GITLAB_BASE_URL`
- `CX_GITLAB_CLIENT_ID`
- `CX_GITLAB_CALLBACK_URL`
- `CX_GITLAB_SCOPES`

For release builds, CI should set:

- `CX_ENFORCE_EMBEDDED_SECRETS=1`
- `GITLAB_RELEASE_TOKEN` (GitLab token with API scope for creating releases)

If the secret variables are absent in local development, the build falls back to
placeholder values so `cargo test` still works.

For `codex`, only models verified to work through the injected DashScope
responses provider are exposed in the embedded config.

## GitLab CI delivery

`.gitlab-ci.yml` provides:

1. `cargo fmt --check`
2. `cargo test`
3. release binary build jobs
4. upload to GitLab Generic Package Registry
5. GitLab Release creation with permanent asset links
6. npm package publish to the GitLab npm registry

## Local development

```bash
./scripts/build.sh
cargo test
```

For a local binary install from source:

```bash
./scripts/install.sh
```
