<img src="assets/logo.svg" alt="Roch GPU" width="560">

# Roch GPU

A native Rust tuning and monitoring tool for NVIDIA, AMD and Intel Arc GPUs on Windows, and AMD GPUs on Linux.

**[Download for Windows](https://github.com/RochStudio/Roch-GPU/releases/latest)** · **[Install on Linux](docs/linux.md)**

On Windows, run `RochGPU.exe` and allow administrator access when prompted.

- GPU tuning, fan controls, live telemetry and saved profiles.
- Custom themes, colours, fonts and layouts through the Settings gear.
- One executable. No .NET or HWiNFO required.
- The same window and command line on Linux, on Wayland or X11.

## Supported GPUs

| GPU | Windows | Linux | Tested on |
|---|---|---|---|
| NVIDIA RTX 50 series | Yes | No | RTX 5070 Ti |
| NVIDIA RTX 40 series | Yes | No | RTX 4070 Ti, RTX 4070 |
| AMD RDNA 4 (RX 9000 series) | Yes | Yes, not yet run on a real card | RX 9070 XT, on Windows |
| AMD RDNA 3 (RX 7000 series) | Yes | Yes | RX 7900 XT, on Windows and Linux |
| Intel Arc | Yes | No | Arc Pro B60 |

Support varies by GPU and driver, and other cards may work. See the [technical guide](docs/development.md#verification-status) for hardware validation and tuning limitations.

## Build from source

With Rust stable:

```console
cargo xtask build
```

On Linux, `cargo xtask install` puts `rochgpu` and a launcher entry in `~/.local`. See the [Linux guide](docs/linux.md) for the one step that needs root.

---

Created by **Roch Studio**. GPL-3.0-or-later · [Technical guide](docs/development.md) · [Third-party notices](THIRD-PARTY-NOTICES.md)

[YouTube](https://www.youtube.com/@MateoPcTech) | [X](https://x.com/MateoPCTech) | [Discord](https://discord.gg/KfzExpKQHB)
