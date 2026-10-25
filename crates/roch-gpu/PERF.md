# Performance notes

Measured on the development machine (Windows 11, RTX 5070 Ti, driver 616.92), release builds.
"Before" is `rust-rewrite` at 0bdc89d plus only the timing trace (commit 05b9f60), which does
nothing unless switched on. "After" is the `perf` branch. The GUI was only ever run with `--mock`
and `APPDATA` pointed at a scratch folder. The only verbs run against the real card were the
read-only `info`, `sensors` and `domains`.

## Results

| | Before | After |
|---|---|---|
| `RochGPU.exe` size | 8,797,696 B (8.39 MiB) | 6,323,712 B (6.03 MiB), **−28%** |
| `.text` (cargo bloat) | 6.1 MiB | 4.6 MiB |
| CLI `info --mock`, process lifetime (median of 20) | 62.4 ms | **3.2 ms** |
| CLI `info`, real card (median of 8, two rounds) | 248.5 / 249.6 ms | **183.1 / 184.3 ms** |
| CLI `sensors`, real card (median of 8, two rounds) | 187.1 / 187.5 ms | **125.1 / 124.2 ms** |
| GUI `--mock`, process start → first frame presented (median of 15) | 120.2 ms | **107.4 ms** |
| …of which eframe window + GL context | 88.8 ms (4.2 → 93.0) | 89.4 ms, now overlapped with the driver half (below) |
| …driver half (backend, recovery, view model incl. `schtasks /Query`) | 11.5 ms, before the window | runs beside it, on a thread |
| Telemetry open: first frame (window create + paint) | 5.3–6.6 ms | 5.2–6.1 ms |
| Telemetry open: first fresh sample on screen | 97–115 ms in these runs; anywhere up to the 1 s poll interval | **28–33 ms** (first open), **8 ms** (reopen) |
| Telemetry close (next frame) | 0.2 ms | 0.2 ms |
| Open V/F Curve / XOC / Fan / Graphics (first frame) | 6.2–6.6 / 5.4–5.7 / 5.5–5.6 / 4.7–5.6 ms | 6.6–6.7 / 5.6–5.8 / 6.0–7.3 / 5.4–5.7 ms |
| Main window idle, 10 s | 1 frame, 0 ms CPU | 1 frame, 0 ms CPU |
| Telemetry open and idle, 10 s | **21 frames**, 94–125 ms CPU (0.9–1.25% of a core) | **11 frames**, 0–31 ms CPU (≤0.3%) |
| Working set at first frame | 63.4 MiB | 62.4 MiB |
| Working set, Telemetry open | 77 MiB | 76 MiB |

Notes on the numbers:

* CPU time comes from `GetProcessTimes`, which ticks in 15.6 ms steps. Take the idle-CPU figures
  as "about 6–8 ticks" before and "0–2 ticks" after.
* About one run in four, in both builds, starts with a working set near 128 MiB instead of 63–69
  MiB. Most of the memory is the NVIDIA OpenGL driver: it jumps by ~42 MiB while eframe creates
  the GL context. The table uses the typical runs.
* Real-card CLI times change with the GPU's power state. The before and after runs were
  interleaved. Roughly 96 ms of every real-card run is NVAPI initialisation (`cli: service
  initialised` in the trace), which is backend code and was left alone.
* The child-window rows are the frame in which the window was created and first painted (an
  immediate viewport is created, laid out, painted and swapped inside the root frame). Closing a
  window is the next frame plus eframe destroying the native window. Both were already fast and
  are unchanged within noise.

### cargo bloat (top crates)

| Before (opt 3, thin LTO, default fonts) | | After (opt s, fat LTO) | |
|---|---|---|---|
| std | 1.3 MiB | std | 1023 KiB |
| egui | 848 KiB | egui | 540 KiB |
| harfrust | 413 KiB | fearless_simd | 459 KiB |
| RochGPU | 345 KiB | RochGPU | 293 KiB |
| skrifa | 340 KiB | vello_cpu | 277 KiB |
| vello_cpu | 304 KiB | harfrust | 265 KiB |
| fearless_simd | 266 KiB | skrifa | 252 KiB |
| epaint | 239 KiB | epaint | 168 KiB |
| winit | 218 KiB | read_fonts | 148 KiB |
| roch_nvidia | 203 KiB | roch_core | 140 KiB |
| roch_core | 192 KiB | eframe | 133 KiB |
| read_fonts | 190 KiB | roch_nvidia | 124 KiB |

egui's ~0.7 MB of embedded fonts sat in `.rdata`, which `--crates` does not show.

## How to measure

### UI customization follow-up (October 24, 2026)

A local mock comparison exposed stalls after adding window preferences. Native title lookup
enumerated every window in the process; `GetWindowTextW` can synchronously message windows on
driver/helper threads. Lookup now uses `EnumThreadWindows` on the UI thread. Opacity, bounds and
child size constraints are updated only when their inputs change, and renderers share `Arc<Prefs>`.

One full scripted run per build, using separate scratch settings folders and the same 39.5 seconds
of scheduled waits, reached `bench: Exit` at **50.69 s before** and **39.66 s after**. The two
10-second idle phases took 11.3/11.0 seconds before and 10.0/10.0 seconds after. These are local
responsiveness checks, not a claim about frame rate or GPU driver performance. Trace logs are
under `artifacts/RochGPU-lag-benchmark` in the parent workspace.

The timing trace is built in and does nothing unless switched on:

```powershell
$env:APPDATA = "C:\scratch\appdata"            # keep real profiles/settings untouched
$env:ROCH_TRACE = "1"                           # or a file path; default %TEMP%\roch-gpu-trace.log
.\RochGPU.exe info --mock                       # CLI phases
$env:ROCH_TRACE_BENCH = "startup"; .\RochGPU.exe --mock   # exits once the first frame is shown
$env:ROCH_TRACE_BENCH = "1";       .\RochGPU.exe --mock   # ~40 s scripted open/close run
```

Each line is `ms since process creation`, the working set, and a label. Labels:
`gui: run_native`, `gui: eframe created`, `gui: driver startup joined`, `first frame drawn`,
`first frame presented`, one `frame #N` line per root frame (whole frame, the main window's
part, and which child windows are open), `telemetry: first sample arrived`, and
`bench: idle 10.0 s: N frames, CPU …` from the scripted run. The scripted run only works with
`--mock` and stays out of the single-instance handshake. CLI lifetimes above are
`Process.ExitTime − StartTime` over repeated runs.

## Changes (in commit order)

1. **Timing trace** (`src/trace.rs`, `gui/bench.rs`): described above. When off, each call site
   is one `OnceLock` load.
2. **Delay-load `opengl32.dll`** (`build.rs`: `/DELAYLOAD:opengl32.dll` + `delayimp.lib`). Just
   having opengl32 in the import table cost every process about 55 ms in `DLL_PROCESS_DETACH` at
   exit. A bare test exe took 62 ms with the import and 2.6 ms without. Only the window needs
   the DLL, and glutin `LoadLibrary`s it before any wgl call. The CLI gets 55–65 ms faster on
   every verb, and the GUI is unaffected. (`TerminateProcess` at the end of the CLI gave the
   same speed but would also skip nvapi/nvml's detach, so it was rejected.)
3. **Windows fonts only** (`default_fonts` off): the UI already used Consolas, with Segoe UI
   Symbol as the fallback. Every glyph the UI draws (° — – … · × − → ↑ ↓ ◀ ▶ ☀) was checked
   against the fonts' cmaps and visually in every window, in dark mode. If Consolas is missing,
   Segoe UI, Tahoma or Arial stands in. Saves 681 KB.
4. **Overlapped startup** (`gui::Boot`): backend detection and initialisation, the recovery
   journal, the logon apply and `MainViewModel::new` run on a thread while eframe creates the
   hidden window and GL context. The app creator joins the thread before the first frame. The
   order of the steps is unchanged. `--exit` still runs inline. If the window fails before the
   join, the app still waits for the thread, so an apply is never cut short. The `schtasks /Query`
   behind the Startup checkbox starts at once on its own thread (`PrefetchedStartup`: the first
   `exists()` uses that answer; later queries and every change go to schtasks). The tray icon
   is made after the first frame unless the app starts in the tray. The icon PNG is decoded
   once instead of three times. With `--mock`: −12.8 ms. On the real card, NVAPI initialisation
   (~96 ms in the CLI trace) should now hide behind the ~89 ms of GL setup. This is inferred
   from the CLI and mock traces; the GUI was not run against the card.
5. **Telemetry wake-ups**: leaving background mode wakes the poll thread for an immediate sample
   (`PollSignal::Wake` on the existing stop channel, `try_lock` so it never blocks; test
   `leaving_background_mode_samples_at_once`). Before, the first fresh reading could wait for up
   to the whole poll interval. Data from other threads now wakes the UI for **one** frame
   (`gui::wake`) instead of `request_repaint`'s two. The app's per-sample wake waits 30 ms, so the
   Monitor's tap result lands in the same frame. Every root frame also repaints each open child
   viewport, so with Telemetry open this halves the frames.
6. **Release profile** `opt-level = "s"`, `lto = "fat"` (still `codegen-units = 1`, `strip`,
   `panic = "unwind"`). Same commit, bytes / `--mock` first frame:

   | profile | size | first frame |
   |---|---|---|
   | 3 + thin LTO (old) | 8,142,848 | 121.2 ms |
   | 3 + fat | 7,547,392 | 121.7 ms |
   | 2 + fat | 7,519,744 | — |
   | s + thin | 6,624,768 | — |
   | **s + fat** | **6,294,528** | 123.6 ms |
   | z + fat | 6,544,384 | 123.8 ms |
   | s + fat + `panic = "abort"` | 4,882,944 | — |

   Frame 1's UI time rises from 2.0 to 2.5 ms. Real-card `info` is driver-bound and unchanged.
7. **Unused dependencies**: `serde_json` and the `Registry`/`Dwm` windows-sys features in
   roch-gpu. This only affects build time; the exe is byte-identical.

## Tried or considered, and rejected

* **`panic = "abort"`** (−1.4 MB, the largest single item): the poll loop and backend detection
  rely on `catch_unwind`. A panicking driver call must not stop a software fan curve, which
  would leave the fan at its last duty, or stop the next vendor backend from being tried.
  Keeping that safety under abort would mean proving every backend panic-free, so unwind stays.
* **Exe packers (UPX and the like)**: rejected. A compressed, self-modifying exe that asks for
  elevation and writes to the GPU driver is a classic antivirus false positive. It also
  decompresses on every start.
* **`opt-level = "z"`**: larger than `"s"` here (6.54 vs 6.29 MB), and no faster.
* **Keeping child viewports alive but hidden** (`ViewportCommand::Visible(false)`): opening a
  window is a single ~5–7 ms frame and closing one is ~0.2 ms. An immediate viewport has to be
  shown every root frame to stay alive, so hidden windows would be laid out, painted and
  swapped on every frame. That costs more than it saves. Polling is still tied to logical
  visibility as before.
* **Deferred viewports** (child windows repainting on their own): they need `'static + Send`
  UI callbacks, which means rebuilding how every window shares `MainViewModel`. Too large and
  risky for the gain, now that idle frames are rare.
* **Showing the window before the driver is ready**: every main-window field is driver-derived,
  and the view model has no "not loaded yet" state. Overlapping the two (change 4) gets most of
  the win without one.
* **Dropping eframe's `clipboard` feature** (arboard plus image's BMP codec, ~50–80 KB): it would
  stop paste from other apps into value boxes and the profile name.
* **Removing the `image` crate**: eframe depends on `image` (PNG) unconditionally, so swapping
  our three lines of decoding for something else saves nothing.
* **Trimming `fearless_simd` / `vello_cpu`** (egui 0.37's glyph rasteriser, ~0.7 MB together):
  epaint fixes their features (both u8 and f32 pipelines), so this needs an upstream feature.
* **Duplicate `windows-sys` versions** (0.52 via glutin/winit, 0.59 via winreg and ours, 0.61 via
  eframe/muda): these are bindings only, and the linker keeps just the functions used. Deduping
  would mean moving winreg and the backends to another windows-sys version, which costs churn
  and gains no bytes.
* **`TelemetryHistory`**: the service clones every foreground sample into a 120-sample ring that
  nothing in the app reads (only the tests do). It was left alone because it is shared
  `roch-core` service logic, mirrors the C#, and is cheap at 1 Hz. It is a candidate for removal.
* **clippy::pedantic pass**: used for hints only. Its findings are style (docs, casts,
  `must_use`) with nothing on a hot path, so working driver code was not rewritten for it.
