# remote-lab

Personal remote-desktop experiment.

- **`remote-host`** runs on the computer you want to control. It captures the
  screen and accepts mouse / keyboard / scroll over a WebSocket.
- **`remote-viewer`** is a small native (egui) viewer.
- **`web/viewer.html`** is served by the host on the **same port**, so any
  browser (phone, tablet, another laptop) can view + control without
  installing anything.

This is a LAN-trust prototype: no built-in TLS or password. Use only on
networks you trust, or put it behind a tunnel + auth (see "Going over the
internet" below).

---

## Build the host on the OTHER computer

Install Rust via https://rustup.rs (everywhere). Then:

### Windows (10/11)

1. Install Rust: download `rustup-init.exe` from https://rustup.rs and run it.
   Accept the default toolchain (it installs MSVC build tools if missing).
2. Open a fresh **PowerShell** (so `cargo` is on PATH) and `cd` into this
   project folder.
3. Build:
   ```powershell
   cargo build --release --bin remote-host
   ```
4. Run:
   ```powershell
   .\target\release\remote-host.exe --bind 0.0.0.0:9753 --fps 12 --max-width 1280 --jpeg-quality 55
   ```
5. When Windows Firewall asks "Allow remote-host to communicate", click
   **Allow on private networks** (and Public if you want LAN-wide).

### Linux (Debian / Ubuntu)

1. Install Rust:
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   . "$HOME/.cargo/env"
   ```
2. Install build deps for screen capture + input injection:
   ```bash
   sudo apt-get update
   sudo apt-get install -y \
     pkg-config libclang-dev \
     libxcb1-dev libxrandr-dev libxdo-dev \
     libdbus-1-dev libpipewire-0.3-dev libwayland-dev libegl-dev
   ```
3. Build:
   ```bash
   cargo build --release --bin remote-host
   ```
4. Run:
   ```bash
   ./target/release/remote-host --bind 0.0.0.0:9753 --fps 12 --max-width 1280 --jpeg-quality 55
   ```

> **Note:** xcap's screen capture is fully supported on **X11**.
> On **Wayland** it's limited — log out and pick "Ubuntu on Xorg" from the
> session menu if you hit issues capturing.

### Linux (Fedora / RHEL)

```bash
sudo dnf install -y \
  pkgconf-pkg-config clang-devel \
  libxcb-devel libXrandr-devel libxdo-devel \
  dbus-devel pipewire-devel wayland-devel mesa-libEGL-devel
```

Then `cargo build --release --bin remote-host` and run as above.

---

## Test it on the same LAN first

On the host machine note its LAN IP:
- macOS / Linux: `ipconfig getifaddr en0` or `hostname -I`
- Windows: `ipconfig` → look for "IPv4 Address"

From any phone / laptop on the same Wi-Fi:

```
http://<HOST_IP>:9753/
```

You should see a page titled **remote-lab** with the host's screen.

---

## Going over the internet (different Wi-Fi / cellular)

Easiest tunnel: **Cloudflare Quick Tunnel** (free, no account, no port-forward).

1. Install once on the host:
   - Windows: `winget install --id Cloudflare.cloudflared`
   - Debian/Ubuntu: download the `.deb` from
     https://github.com/cloudflare/cloudflared/releases and `sudo dpkg -i ...`
   - Fedora: `.rpm` from same releases page.
2. With the host running on `0.0.0.0:9753`, in another shell:
   ```
   cloudflared tunnel --url http://localhost:9753
   ```
3. cloudflared prints a URL like
   `https://random-words.trycloudflare.com` — open that on any device.

**Security warning.** This URL is public. Anyone who learns it can control
that computer. For a personal experiment, treat the URL like a password and
do not share it. A proper deployment should add a token check and/or
Cloudflare Access — out of scope for this MVP.

---

## CLI flags

```
remote-host [OPTIONS]

  --bind <ADDR>           default 0.0.0.0:9753
  --monitor <INDEX>       default 0
  --fps <N>               default 12
  --jpeg-quality <1-100>  default 60
  --max-width <PX>        default 1280  (0 = capture native resolution)
  --list-monitors         print monitors and exit
```

---

## Troubleshooting

| Symptom | Likely fix |
|---|---|
| Page loads, image stays black | Grant the app Screen Recording permission and **restart** the host |
| Cursor moves but clicks/keys don't | Grant Accessibility permission (macOS) / run as user with input perms (Linux) |
| Phone can connect locally but not via cloudflared URL | Make sure cloudflared is still running; the trycloudflare URL changes each launch |
| Lag is bad | Make sure you built `--release`; lower `--max-width` (e.g. 960) and `--jpeg-quality` (e.g. 45) |
| `cargo: command not found` | Open a new shell or `source $HOME/.cargo/env` |
