# Subtitle-live

Local English live subtitles for selected Ubuntu audio streams.

`Subtitle-live` discovers playback applications through PipeWire, captures only
the applications or streams selected by the user, converts the audio to mono
16 kHz PCM, transcribes it locally with whisper.cpp, and displays the result in
a floating subtitle overlay.

This repository is currently in Phase 1:

```text
Selected PipeWire audio -> English STT -> English live subtitle
```

English-to-Thai translation, cloud transcription, accounts, recording, and
subtitle export are intentionally outside the current scope.

## Project Status

- Ubuntu is the primary platform.
- PipeWire is the primary audio backend; a PulseAudio system-output fallback is
  available.
- `small.en` is the initial model for latency validation.
- AMD ROCm/HIP, NVIDIA CUDA, and CPU builds exist; this guide documents the
  ROCm/HIP path first.
- The settings window runs natively on X11 or Wayland. The subtitle overlay uses
  X11 directly or XWayland on a Wayland desktop.
- Native Wayland overlay protocols are not implemented yet.
- The HIP build works in the current development environment, but clean-machine
  installation and real audio/GPU behavior still need to be verified on each
  target machine.

## Contents

- [ROCm/HIP quick start](#rocmhip-quick-start)
- [Install the build dependencies](#1-install-the-build-dependencies)
- [Install ROCm, HIP, and hipBLAS](#2-install-rocm-hip-and-hipblas)
- [Install Rust](#3-install-rust)
- [Get the source and model](#4-get-the-source-and-model)
- [Build and run](#5-build-and-run)
- [Use the application](#use-the-application)
- [Prepare a release bundle](#prepare-a-release-bundle)
- [Configuration and local data](#configuration-and-local-data)
- [Troubleshooting](#troubleshooting)
- [Development commands](#development-commands)

## ROCm/HIP Quick Start

Use this path after the Ubuntu build dependencies and `rustup` are installed,
and after `hipcc --version` and `rocminfo` work for the current user.

```bash
git clone https://github.com/abdulloh404/subtitle-live.git
cd subtitle-live

rustup toolchain install 1.92.0 --profile minimal
rustup override set 1.92.0

export SUBTITLE_LIVE_HIP_GCC_DIR="$(dirname "$(gcc -print-libgcc-file-name)")"
COMPUTE_BACKEND=rocm make build
make model
COMPUTE_BACKEND=rocm make run
```

`make run` uses Cargo's development profile. Use `make release` for the
optimized binaries.

## 1. Install the Build Dependencies

The current CI build uses Ubuntu 24.04. The project also targets compatible
Ubuntu 22.04 systems, subject to the ROCm support matrix for the exact GPU,
kernel, and ROCm release.

Install the common source-build packages:

```bash
sudo apt update
sudo apt install --yes --no-install-recommends \
  ca-certificates curl git gnupg wget \
  build-essential clang cmake libclang-dev pkg-config \
  libatomic1 libquadmath0 \
  libgtk-4-dev libadwaita-1-dev \
  libpipewire-0.3-dev libspa-0.2-dev \
  pulseaudio-utils xwayland
```

Optional GNOME tray support:

```bash
sudo apt install gnome-shell-extension-appindicator
```

The tray is optional. The settings window and subtitle pipeline can still run
when the desktop has no StatusNotifier/AppIndicator host.

## 2. Install ROCm, HIP, and hipBLAS

`COMPUTE_BACKEND=rocm` enables the `whisper-rs/hipblas` feature. Compiling that
backend requires all of the following, not only the ROCm runtime:

- a supported AMD GPU and a working `amdgpu` kernel driver;
- the HIP runtime and `hipcc` compiler;
- the hipBLAS and rocBLAS shared libraries; and
- HIP, hipBLAS, and rocBLAS headers and CMake package files.

Check the [ROCm compatibility matrix](https://rocm.docs.amd.com/en/latest/compatibility/compatibility-matrix.html)
for the exact GPU, Ubuntu release, and kernel before changing a driver. Radeon
and Ryzen systems can have different kernel requirements; follow AMD's
[hardware-specific guides](https://rocm.docs.amd.com/projects/radeon-ryzen/en/latest/)
instead of replacing the installed driver automatically.

### Do not mix the two ROCm package streams

AMD changed package names and the installation root in ROCm Core SDK 7.14:

| Release stream | Package names | Installation root | Use in this guide |
| --- | --- | --- | --- |
| ROCm Core SDK 7.14 | `amdrocm-*` | `/opt/rocm/core` | Recommended |
| Legacy ROCm 7.2.x | `rocm-*`, `hipblas-*`, `rocblas-*` | `/opt/rocm` | Existing legacy installations only |

Do not install packages from both streams. Migrate or uninstall the legacy
stack first by following AMD's
[ROCm 7.14 transition guide](https://rocm.docs.amd.com/en/latest/about/transition-guide-TheRock.html).
The project helper defaults to `/opt/rocm/core`, so ROCm Core SDK 7.14 needs no
ROCm-root override.

### Recommended installation: ROCm Core SDK 7.14

These commands cover x86-64 Ubuntu 24.04 and 22.04. Confirm the installed
release first:

```bash
uname -m
. /etc/os-release
printf '%s\n' "$PRETTY_NAME"
```

Register AMD's signing key once:

```bash
sudo mkdir --parents --mode=0755 /etc/apt/keyrings
wget https://repo.amd.com/rocm/packages-multi-arch/gpg/rocm.gpg -O - | \
  gpg --dearmor | sudo tee /etc/apt/keyrings/amdrocm.gpg >/dev/null
```

For **Ubuntu 24.04**, register this repository:

```bash
echo 'deb [arch=amd64 signed-by=/etc/apt/keyrings/amdrocm.gpg] https://repo.amd.com/rocm/packages-multi-arch/ubuntu2404 stable main' | \
  sudo tee /etc/apt/sources.list.d/rocm.list
```

For **Ubuntu 22.04**, use this repository instead:

```bash
echo 'deb [arch=amd64 signed-by=/etc/apt/keyrings/amdrocm.gpg] https://repo.amd.com/rocm/packages-multi-arch/ubuntu2204 stable main' | \
  sudo tee /etc/apt/sources.list.d/rocm.list
```

Choose exactly one repository above, then update APT and install the complete
developer SDK:

```bash
sudo apt update
sudo apt install --yes amdrocm-core-sdk7.14
```

This is the simplest option and supports every GPU architecture packaged in
the release, but it uses the most disk space. AMD's
[hipBLAS installation guide](https://rocm.docs.amd.com/projects/hipBLAS/en/latest/install/install.html)
recommends `amdrocm-core-sdk` for a complete developer installation.

If the exact GPU target is already known, the developer-essentials package is
smaller and still contains the compiler, runtime, headers, CMake files,
hipBLAS, and rocBLAS needed by Subtitle-live. For example, use this only for a
GPU that AMD identifies as `gfx1100`:

```bash
sudo apt install --yes amdrocm-core-dev7.14-gfx1100
```

Replace `gfx1100` only with a target listed for the GPU in AMD's compatibility
matrix. If the target is uncertain, use the all-architecture SDK above. Do not
install `amdrocm-blas-dev7.14` by itself: it supplies BLAS development files,
but it is not a complete HIP compiler and GPU runtime installation. In the
7.14 package stream, `hipcc` is provided by `amdrocm-llvm`, while hipBLAS and
rocBLAS are consolidated under `amdrocm-blas`.

### Legacy ROCm 7.2.x installation

Use this only when the machine is intentionally kept on AMD's legacy 7.2.x
repository. The appropriate developer meta-package is:

```bash
sudo apt install --yes rocm-hip-sdk
```

`rocm-hip-sdk` includes `hipcc`, `hipblas-dev`, `rocblas-dev`, their runtime
libraries, headers, and CMake files. Installing only `hipblas` or
`rocm-hip-runtime` is insufficient for compiling Subtitle-live. Follow AMD's
[legacy package details](https://rocm.docs.amd.com/projects/install-on-linux/en/latest/reference/package-manager-integration.html)
when maintaining this stream.

### Configure GPU access and the shell

Add the current user to the groups that own the KFD and DRM render devices:

```bash
sudo usermod -a -G render,video "$LOGNAME"
```

Reboot, or log out of the entire desktop session and log back in, before
continuing. Then expose the ROCm Core SDK commands in the current shell:

```bash
export PATH="/opt/rocm/core/bin:${PATH}"
```

For a legacy `/opt/rocm` installation, use these overrides instead:

```bash
export PATH="/opt/rocm/bin:${PATH}"
export SUBTITLE_LIVE_ROCM_CORE=/opt/rocm
export SUBTITLE_LIVE_LIBCLANG_PATH=/opt/rocm/llvm/lib
```

The build helper currently defaults to GCC 11. Select the GCC installation
that is actually active on the machine:

```bash
export SUBTITLE_LIVE_HIP_GCC_DIR="$(dirname "$(gcc -print-libgcc-file-name)")"
```

### Verify HIP and hipBLAS before building

For ROCm Core SDK 7.14, all commands below must succeed:

```bash
ROCM_ROOT=/opt/rocm/core

groups
ls -l /dev/kfd /dev/dri/renderD*
"$ROCM_ROOT/bin/hipcc" --version
"$ROCM_ROOT/bin/rocminfo" | grep -E 'Name:[[:space:]]+gfx|Marketing Name'

ls -l \
  "$ROCM_ROOT/include/hipblas/hipblas.h" \
  "$ROCM_ROOT/lib/cmake/hip/hip-config.cmake" \
  "$ROCM_ROOT/lib/cmake/hipblas/hipblas-config.cmake" \
  "$ROCM_ROOT/lib/cmake/rocblas/rocblas-config.cmake" \
  "$ROCM_ROOT/lib/libhipblas.so" \
  "$ROCM_ROOT/lib/librocblas.so"

ldconfig -p | grep -E 'lib(hip|roc)blas\.so'
```

For a legacy installation, set `ROCM_ROOT=/opt/rocm` before running the same
checks. If a header, CMake file, or unversioned `.so` link is missing, install
the development meta-package for the selected stream instead of working around
the missing file with a manual symlink.

## 3. Install Rust

The crate requires Rust 1.92 or newer. Install the pinned toolchain with
`rustup`; Ubuntu's distribution package may be too old.

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustup toolchain install 1.92.0 --profile minimal --component rustfmt --component clippy
rustup override set 1.92.0
rustc --version
cargo --version
```

Run `rustup override set` inside the repository so other Rust projects are not
forced onto this version.

## 4. Get the Source and Model

```bash
git clone https://github.com/abdulloh404/subtitle-live.git
cd subtitle-live
```

Download the default `small.en` whisper.cpp model:

```bash
make model
```

The download script pins the source revision, verifies SHA-256, and installs
the file under `~/.subtitle-live/models`.

Other benchmark models are available but require more disk space and memory:

```bash
MODEL=medium.en make model
MODEL=large-v3-turbo make model
```

| Model | Download size | Intended use |
| --- | ---: | --- |
| `small.en` | 488 MB | Initial latency validation |
| `medium.en` | 1.53 GB | Accuracy/latency comparison |
| `large-v3-turbo` | 1.62 GB | Accuracy/latency comparison |

Models can also be selected and downloaded from **Speech Recognition** in the
settings window.

## 5. Build and Run

### ROCm/HIP development build

Force the backend for a reproducible HIP build:

```bash
export SUBTITLE_LIVE_HIP_GCC_DIR="$(dirname "$(gcc -print-libgcc-file-name)")"
COMPUTE_BACKEND=rocm make build
COMPUTE_BACKEND=rocm make run
```

The explicit `COMPUTE_BACKEND=rocm` is recommended even though the Makefile can
detect AMD display hardware. Hardware detection cannot prove that a compatible
ROCm SDK is installed.

### Optimized ROCm/HIP build

```bash
COMPUTE_BACKEND=rocm make release
./target/release/subtitle-live
```

The release build produces two executables:

```text
target/release/subtitle-live
target/release/subtitle-live-overlay
```

Keep both executables together when preparing a bundle. The overlay executable
is the dedicated renderer used by release installations.

### CPU fallback

If ROCm is not ready, build and run the CPU path explicitly:

```bash
COMPUTE_BACKEND=cpu make run
```

A ROCm-enabled binary can also use CPU inference through **Speech Recognition →
Compute Backend**. CUDA and ROCm remain mutually exclusive compile-time
features; a single binary cannot contain both GPU backends.

## Use the Application

1. Start English playback in the browser or media application to caption.
2. Launch Subtitle-live with `COMPUTE_BACKEND=rocm make run` or the release
   executable.
3. Open **Speech Recognition**:
   - Confirm the selected model reports **Ready**.
   - Select **ROCm / HIP** under **Compute Backend**.
   - Confirm **Binary Acceleration** reports ROCm/HIP.
4. Open **Audio Sources**:
   - Keep **PipeWire** selected for per-application and per-stream capture.
   - Enable an entire playback application, or expand it and enable individual
     streams.
5. Open **General** and enable **Live Subtitles**.
6. Use **Subtitle** to choose the display, position, alignment, size, width,
   opacity, and line count.
7. Use **Performance** to inspect pipeline state, the latest error, audio-buffer
   delay, Whisper inference time, end-to-end delay, and dropped frames.
8. Disable **Live Subtitles** or use **Stop Subtitles** from the tray when done.
   Use **Quit** from the tray to terminate the application completely.

Closing the settings window either hides the window or quits the application,
depending on **Keep Running When Closed**. Clicking the tray icon reopens the
settings window when tray integration is available.

### Audio backends

- **PipeWire** is the primary backend. It preserves application and stream
  selections using stable metadata where available. PulseAudio clients routed
  through `pipewire-pulse` appear in the same graph.
- **PulseAudio** is a compatibility fallback for a native PulseAudio daemon. It
  captures the default system-output monitor as one source and does not provide
  per-application isolation.

When using the PulseAudio fallback, select **Default system output** before
starting subtitles.

### Overlay backends

- X11 sessions use the X11/EWMH overlay backend.
- Wayland sessions require XWayland and a valid `DISPLAY` value for the overlay.
- The main settings window remains available if the overlay cannot start.
- Native Wayland layer-shell support is not implemented.

## Prepare a Release Bundle

There is no production `.deb` package in this repository yet. The project does
not currently ship Debian metadata, a desktop entry, an application icon, or a
redistribution license file. Do not publish a binary package until those items
and third-party notices are completed and reviewed.

For local testing on a machine with a matching Ubuntu and ROCm ABI, create a
binary bundle:

```bash
COMPUTE_BACKEND=rocm make release

bundle_dir="dist/subtitle-live-0.1.0-linux-x86_64-rocm"
install -d "${bundle_dir}/bin"
install -m 0755 target/release/subtitle-live "${bundle_dir}/bin/"
install -m 0755 target/release/subtitle-live-overlay "${bundle_dir}/bin/"
install -m 0644 README.md "${bundle_dir}/README.md"

tar -C dist -czf dist/subtitle-live-0.1.0-linux-x86_64-rocm.tar.gz \
  subtitle-live-0.1.0-linux-x86_64-rocm
sha256sum dist/subtitle-live-0.1.0-linux-x86_64-rocm.tar.gz
```

This bundle is dynamically linked. It does not include ROCm, GTK, Libadwaita,
PipeWire, system libraries, or a Whisper model. Inspect both executables before
moving the bundle to another machine:

```bash
ldd target/release/subtitle-live
ldd target/release/subtitle-live-overlay
```

Every dependency must resolve without `not found`, and the destination machine
must use a compatible ROCm stack.

For a simple machine-local installation without desktop integration:

```bash
sudo install -d /opt/subtitle-live/bin
sudo install -m 0755 target/release/subtitle-live /opt/subtitle-live/bin/
sudo install -m 0755 target/release/subtitle-live-overlay /opt/subtitle-live/bin/
/opt/subtitle-live/bin/subtitle-live
```

The model remains user-specific under `~/.subtitle-live/models`; do not bundle
the 488 MB model into the application archive by default.

Before calling a future `.deb` release-ready, verify at minimum:

- clean installation, upgrade, removal, and purge on supported Ubuntu versions;
- exact runtime dependencies for GTK, Libadwaita, PipeWire, XWayland, and ROCm;
- desktop entry, icon, tray behavior, and overlay helper placement;
- model download and checksum behavior for a non-developer user;
- PipeWire selection isolation and real AMD GPU inference;
- license, copyright, and third-party redistribution requirements.

## Configuration and Local Data

| Data | Default location |
| --- | --- |
| Configuration | `$XDG_CONFIG_HOME/subtitle-live/config.toml` or `~/.config/subtitle-live/config.toml` |
| Whisper models | `~/.subtitle-live/models/` |
| Optional transcript debug log | `~/.subtitle-live/logs/transcript-debug.jsonl` |

Audio and transcription stay local. Captured audio is not written to disk by
the normal pipeline. Raw transcript debug collection is disabled at every app
launch; enabling **Write Debug Log File** can store sensitive transcript text,
so enable it only when needed.

Normal process logs are structured JSON on standard output. Increase logging
for diagnosis without enabling raw transcript capture:

```bash
RUST_LOG=subtitle_live=debug COMPUTE_BACKEND=rocm make run
```

## Troubleshooting

### `hipcc` is not found

Complete the ROCm installation and ensure the active ROCm `bin` directory is on
`PATH`:

```bash
export PATH="/opt/rocm/core/bin:${PATH}"
hipcc --version
```

Use `/opt/rocm/bin` only for a legacy ROCm installation.

### CMake cannot find HIP, hipBLAS, or rocBLAS

Point the project helper at the ROCm root that contains the CMake package files:

```bash
export SUBTITLE_LIVE_ROCM_CORE=/opt/rocm/core
COMPUTE_BACKEND=rocm make build
```

If `/opt/rocm/core/lib/cmake/hipblas/hipblas-config.cmake` or
`rocblas/rocblas-config.cmake` is absent, the ROCm development packages are
incomplete. Install `amdrocm-core-sdk7.14` or the correct architecture-specific
`amdrocm-core-dev7.14-*` package. Legacy ROCm users should use
`SUBTITLE_LIVE_ROCM_CORE=/opt/rocm`.

### Bindgen cannot load `libclang`

Set the directory containing the ROCm or system `libclang` shared library:

```bash
export SUBTITLE_LIVE_LIBCLANG_PATH=/opt/rocm/core/llvm/lib
COMPUTE_BACKEND=rocm make build
```

Use `/opt/rocm/llvm/lib` only for a legacy ROCm installation.

### HIP selects the wrong host GCC

The helper defaults to GCC 11 for the current development machine. Select the
installed GCC directory explicitly on other Ubuntu releases:

```bash
gcc -print-libgcc-file-name
export SUBTITLE_LIVE_HIP_GCC_DIR="$(dirname "$(gcc -print-libgcc-file-name)")"
COMPUTE_BACKEND=rocm make build
```

### ROCm is compiled in but unavailable in the UI

The runtime requires `/dev/kfd` and an AMD DRM render node. Check access and
group membership:

```bash
groups
ls -l /dev/kfd /dev/dri/renderD*
rocminfo | grep -i "Marketing Name:"
```

After adding the user to `video` and `render`, log out and back in or reboot.

### The model is missing

```bash
make model
ls -lh "$HOME/.subtitle-live/models/ggml-small.en.bin"
```

The **Speech Recognition** page can also download or retry the selected model.

### No applications appear under Audio Sources

- Start playback first; idle applications might not expose a playback stream.
- Keep PipeWire selected when per-application selection is required.
- Confirm the desktop audio graph is available with `wpctl status`.
- Use the PulseAudio fallback only when the system is running a native
  PulseAudio daemon.

### The settings window opens but no subtitle overlay appears

```bash
echo "$XDG_SESSION_TYPE"
echo "$DISPLAY"
```

On Wayland, install or enable XWayland. The **About** page reports the detected
desktop session, settings backend, overlay backend, and XWayland availability.

### AMD hardware is detected but ROCm is not installed

The automatic build detector may select ROCm from the PCI device alone. Force a
CPU build until ROCm is ready:

```bash
COMPUTE_BACKEND=cpu make run
```

## Development Commands

| Command | Purpose |
| --- | --- |
| `COMPUTE_BACKEND=rocm make build` | Build the development binaries with ROCm/HIP |
| `COMPUTE_BACKEND=rocm make release` | Build optimized ROCm/HIP binaries |
| `COMPUTE_BACKEND=rocm make run` | Build and run the development app |
| `COMPUTE_BACKEND=cpu make build` | Build without GPU features |
| `make model` | Download and verify `small.en` |
| `MODEL=medium.en make model` | Download and verify another supported model |
| `make format` | Format Rust sources |
| `COMPUTE_BACKEND=rocm make lint` | Run Clippy with warnings denied |
| `COMPUTE_BACKEND=rocm make test` | Run all Rust test targets |

The Makefile auto-detects `cuda`, `rocm`, or `cpu` when `COMPUTE_BACKEND` is not
set. Explicit selection is preferred for release and benchmark work.

## Architecture Summary

```text
GTK4 / Libadwaita UI
        |
        v
Application controller
        |
        +--> PipeWire discovery and selected-stream capture
        |        |
        |        v
        |    bounded audio queue -> mono 16 kHz mixer
        |                              |
        |                              v
        |                        whisper.cpp STT
        |                              |
        |                              v
        +<-- events and metrics <- transcript reconciler
                                       |
                                       v
                              X11/XWayland overlay
```

Blocking audio, model loading, inference, and file work run outside the GTK
main thread. Unselected streams must never be sent to STT, raw captured audio is
never logged, and the project remains local-first.

## Phase 1 Success Gate

Before starting Thai translation, the English pipeline should demonstrate:

1. reliable selection of one or more currently playing applications;
2. no obvious capture from unselected applications;
3. continuous English subtitles with measured component and end-to-end latency;
4. practical perceived latency on the target AMD machine;
5. stable long-running behavior without uncontrolled memory/VRAM growth,
   deadlocks, or stream loss.

The final default Whisper model must be chosen from measurements of `small.en`,
`medium.en`, and `large-v3-turbo`, not from assumptions.
