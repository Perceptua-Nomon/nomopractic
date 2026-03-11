# Release & Deploy Guide

This document describes the end-to-end workflow for cutting a release and
deploying the `nomopractic` binary to a Raspberry Pi from a dev machine.

---

## Prerequisites

| Tool | Purpose |
|------|---------|
| `git` | Tag the release |
| `cargo` / `cross` | Local build verification (optional before tagging) |
| `curl`, `sha256sum` | Used internally by the deploy script |
| SSH access to the Pi | Required for remote deploy |

Ensure your SSH key is authorised on the Pi, or set `NOMON_SSH_KEY` to the
path of the relevant private key.

---

## 1 — Pre-release checklist

Before tagging, confirm everything is in order:

```bash
# From the repo root
cargo fmt --all -- --check   # no formatting issues
cargo clippy --all-targets -- -D warnings  # zero warnings
cargo test --all             # all 82+ tests pass
```

Or use the Makefile shortcut:

```bash
make check
```

---

## 2 — Tag and push

Releases are driven by `v*` Git tags. CI will build the `aarch64-unknown-linux-gnu`
binary, compute a SHA-256 checksum, and attach both files to the GitHub Release
automatically when the tag is pushed.

```bash
# Bump the version in Cargo.toml first, then commit.
git add Cargo.toml
git commit -m "chore: bump version to 0.2.0"

# Create a signed (or lightweight) tag.
git tag -a v0.2.0 -m "Release v0.2.0"

# Push the commit and tag together.
git push origin main --follow-tags
```

Once pushed, the **CI → cross → release** workflow chain runs automatically.
Monitor progress at:

```
https://github.com/Perceptua-Nomon/nomopractic/actions
```

The release is ready when the `Publish GitHub Release` job completes (~5–8 min
on the GitHub-hosted runner). The release page will have two assets:

```
nomopractic-v0.2.0-aarch64-linux
nomopractic-v0.2.0-aarch64-linux.sha256
```

---

## 3 — Deploy to the Pi

`scripts/deploy.sh` handles download, verification, atomic installation, and
service restart in a single command.

### 3a — Remote deploy from the dev machine (recommended)

```bash
./scripts/deploy.sh v0.2.0 pi@raspberrypi.local
```

The script will:
1. Download the binary and SHA-256 from GitHub Releases to a local temp directory.
2. Verify the checksum locally — aborts before touching the Pi if it fails.
3. SCP the verified binary to the Pi.
4. On the Pi (over SSH): atomic swap → `systemctl restart nomopractic` → health check.

#### With a specific SSH key

```bash
NOMON_SSH_KEY=~/.ssh/pi_deploy ./scripts/deploy.sh v0.2.0 pi@raspberrypi.local
```

#### Use an env var instead of a positional argument

```bash
export NOMON_PI_HOST=pi@raspberrypi.local
./scripts/deploy.sh v0.2.0
```

### 3b — Deploy directly on the Pi

If you are already logged in to the Pi, copy the script once and run it locally:

```bash
# Copy the script (one-time setup)
scp scripts/deploy.sh pi@raspberrypi.local:~/deploy-nomopractic.sh

# On the Pi
~/deploy-nomopractic.sh v0.2.0
```

---

## 4 — Verify the deployment

```bash
# Check service status
ssh pi@raspberrypi.local "systemctl status nomopractic"

# Smoke-test the IPC socket (on the Pi)
ssh pi@raspberrypi.local \
  "echo '{\"id\":\"1\",\"method\":\"health\",\"params\":{}}' | \
   socat - UNIX-CONNECT:/run/nomopractic/nomopractic.sock"
```

The health response should include the new `version` field matching the tag
you just deployed.

---

## 5 — Rollback

If the new binary fails to start or behaves incorrectly, revert to the
previous binary (automatically saved as `nomopractic.bak`):

```bash
ssh pi@raspberrypi.local \
  "sudo cp /usr/local/bin/nomopractic.bak /usr/local/bin/nomopractic && \
   sudo systemctl restart nomopractic"
```

Then verify the service is healthy as above.

---

## 6 — Environment variable reference

| Variable | Default | Purpose |
|----------|---------|---------|
| `NOMON_PI_HOST` | _(none)_ | SSH target; overridden by the second positional arg |
| `NOMON_SSH_KEY` | _(none)_ | Path to SSH private key for the deploy connection |
| `NOMON_GITHUB_REPO` | `Perceptua-Nomon/nomopractic` | GitHub `owner/repo` slug for asset downloads |
| `NOMON_HAT_SOCKET_PATH` | `/run/nomopractic/nomopractic.sock` | Socket path used by `nomothetic.hat.HatClient` |

---

## 7 — Troubleshooting

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| `checksum mismatch` | Partial download or wrong asset name | Re-run; confirm the GitHub Release job completed |
| Service exits immediately | Config file missing or malformed | Check `/etc/nomopractic/config.toml`; copy from `config.toml.example` |
| Socket not created | Service started as wrong user | Confirm `nomopractic.service` `User=` matches the socket directory owner |
| `HARDWARE_ERROR` on all IPC calls | I2C bus not enabled or wrong address | `sudo raspi-config` → enable I2C; verify `hat_address = 0x14` in config |
| nomothetic returns 503 on `/api/hat/*` | `nomopractic.service` not running | `sudo systemctl start nomopractic` then retry |
