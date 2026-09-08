# Driver notes

What the NVIDIA driver on a 5070 Ti (610.88) and a 4070 Ti (591.86) actually does, established by
measurement rather than documentation. Split out of the README, which people read to use the tool
rather than to work on it.

Every claim here is something that was tried. Where a thing does not work, the note says how that was
established, because knowing where something is not is worth as much as finding it — most of these
began as an attempt to add a feature that turned out not to be there.

`RochGPU.exe diag` prints the raw material behind all of it, and
[docs/diag-rtx5070ti.txt](diag-rtx5070ti.txt) is a sample dump from the card these were taken on.

---

- **The SYS and video clock offsets both work, and neither is worth much yet.** Video was confirmed
  against third-party monitoring. SYS is idle-gated, which made it look inert at first: the domain
  sits pinned near 660 MHz with nothing to do, and an offset of +300 changes that by nothing at all.
  Under load it runs about 2378 MHz and the offset lands on the MHz — +105 measured +104, +300
  measured +299, and returning to zero came back to 2374. It buys no frames on the one workload
  tested, 3DMark 11 Graphics Test 2 at 1440p scoring 74.99 against 75.01, so the clock moves and the
  bottleneck is elsewhere.
  Check a domain with `RochGPU.exe domains`, which reads each one's own counter — but **check it
  under load**. Monitoring tools do not expose the SYS clock, and an idle reading of it says nothing.
- **The crossbar clock is tunable on Blackwell, read-only on Ada.** A 5070 Ti takes a +30 MHz offset
  and reads it back. A 4070 Ti reports a ±1000 MHz range and refuses every non-zero value while 0
  succeeds — a rejection of the value, not of the request shape, so that range is the width of the
  delta field rather than a promise.
- **MSVDD is Blackwell-only.** The rail control is present on a 5070 Ti and absent on the 40-series
  cards tested, where NVVDD works on its own.
- **Hot spot and memory chip temperature are not exposed on Blackwell.** Not a gap in the reading:
  the thermal call has three versions, and the driver names them itself when handed a wrong one
  (`Ver-1:1003c Ver-2:200a8 Ver-3:334c8`, packing as version|size). v2 is the 0xA8 struct already in
  use; v3 is 0x34C8 bytes and wants its mask at +0x08 rather than +0x04, laying out eight channels
  of 0x8C as a reading plus a type. Read that way, a 5070 Ti populates exactly two of the eight —
  GPU and memory junction — with 255 °C in the rest, the same marker v2 uses for an absent sensor.
  Hot spot and the memory chip reading are in neither version. `RochGPU.exe diag` prints all eight
  slots, so a card that populates more will show it — [a sample dump from this
  card](docs/diag-rtx5070ti.txt) is kept as the evidence behind these findings.
- **Ten clock domains, and the info struct only lists nine of them.** Core, crossbar, SYS and video
  have their own controls; HUBCLK, DISPCLK, L2CLK and the reference clock are read-only readings in
  the monitor. The names come from mVolt's telemetry tab and were checked rather than trusted —
  reading both tools at once, their figures and ours agree to within about 2 MHz on every domain
  (HUBCLK 539/540, DISPCLK 674/676, L2CLK 1679/1681, reference 107/108), and type 20 tracks the core
  clock under load exactly as an L2 clock should.
  The reference clock is the reason this list is built by asking the frequency counter for every type
  id rather than by walking the info struct: the walk misses type 22 entirely, though the counter
  answers for it perfectly well. Both are used, because the walk carries type 31 — a domain that
  reads a flat zero, which a probe keeping only what moves would drop. Type 31 stays unnamed; mVolt
  does not name it either, and a number with no name is more honest than a name with no evidence.
- **The crossbar is a single flat offset, not a curve.** HYDRA 2.3B carries a 127-entry
  `xbar_curve_points` array beside its 127 `curve_points`, which suggests a per-voltage-point
  crossbar table. There isn't one on this driver, and all three places it could live were checked:
  the 127-point V/F space is entirely the core curve (its points continue monotonically to 1240 mV /
  3247 MHz, with no second domain in it); each domain's 772-byte control block holds one type word
  and the flat offset field this tool already writes; each domain's 1072-byte info entry holds 12-13
  scattered scalars with a longest run of 4 — a per-point table would be a run of about 127.
  HYDRA's own saved profile has that array all-zero, so it has never written one either.
  `RochGPU.exe diag` prints all four domain blocks and info entries, so a card that does carry a
  table will show it as a long run.
- **No temperature limit on Blackwell, and the power limit's ceiling is the driver's own.** HYDRA
  offers a 90 °C limit and power to 150 %; on a 5070 Ti neither is something the driver will
  discuss. The thermal policy family answers both of its entry points (`ClientThermalPoliciesGetInfo`
  and `GetLimit`) with zero policies, in both struct versions it accepts (v1 and v2 — it refuses
  anything newer, with no list of alternatives), and pre-filling the count does not change that, so
  it is not the mask trap. HYDRA's native helper embeds only the *Set* entry point for that family
  and none of the reads: it writes a limit blind, with the same v2 struct, and never asks whether
  there was a policy to write to. Power is the same shape one step over — the info struct reports
  83.3 / 100 / 116.7 % and accepts only v1, so there is no newer version carrying a bigger number,
  while HYDRA embeds only `SetStatus` and pairs it with `NvAPI_RestartDisplayDriver`. A write past
  the reported maximum was tried, elevated, at idle, with the same v1 struct that works at the
  maximum: 120 % and 150 % both come back `NVAPI_INVALID_ARGUMENT` (-5) and the read-back stays at
  116 667, while 116 667 itself is accepted (status 0) as the control. The driver refuses rather
  than clamps, so HYDRA's 150 % cannot be landing through this call either; whatever its driver
  restart is for, it is not this. `RochGPU.exe diag` prints the read-only probe under *Policy
  shapes*.
- **The sub-volt rail currents are not readable, so the OCP limits have no live figure beside them.**
  The power monitor family (`0xF40238EF`) is decoded — current at +0x00 in milliamps, voltage at
  +0x04 in microvolts — and checked against mVolt+ reading the same card under the same load, where
  every channel lines up one for one: board total 25.75 A against its 26.375, PCIe 12V 0.58
  against 0.585, and the rest within a few per cent. The word at +0x04 is a **bitmask, not a count**,
  and **bit 6 is invalid** — every mask containing it is refused (0x40, 0x7F, 0xFF all fail; 0x3F and
  0x80 both work), which made the family look six wide until that was spotted. `0xBF` is every valid
  bit and yields **seven** channels; bits from 8 up are refused, so seven is all there is, and all
  seven are on the 12 V side.

  The channels an OCP limit actually guards — NVVDD output and MSVDD at about 1.05 V, which reach
  **286 A** under load against a 300 A limit — are in neither that family nor the ADC family, whose
  entries carry a voltage and no current. Nor is HYDRA a way in: its exported
  `NvApi_GetPowerRailSnapshot` compiles to the same implementation calling these same two ids, so it
  sees the same seven. And the OCP family's own fourth entry point — `0x67F31384`, struct 0xA70
  version 4, the range call — reports the live current as **−1**: it carries a minimum, a default
  and a maximum per channel and no reading. mVolt+ shows twelve channels and keeps no plaintext
  entry points, so unlike HYDRA there is no binary to read the answer out of. Until that changes, the
  OCP sliders set a limit with no live current to compare against.
- **Only one of the two OCP limits gets its window from the driver.** That range call reports the
  core limit as min 250 A, default 300 A, max 350 A, and the sliders now use it. For MSVDD the only
  entry it returns is 1 A to 5001 A — the driver declining to constrain it rather than a window
  worth offering — so that slider falls back to half the stock figure to half again above it, 60 to
  180 A. Values the driver refuses are reported rather than silently dropped, so the fallback is
  safe; it is just not the card's own number.
- **There is no per-rail power limit, only a board one.** `ClientPowerPoliciesGetInfo` returns
  exactly one entry on a 5070 Ti (P0, min 83.3 %, default 100 %, max 116.7 %) and `GetStatus`
  returns a count of one. So MSVDD has an over-current limit in amps and no power limit of its own,
  and neither does NVVDD — the percentage on the main window governs the whole board.
- **A clock range cannot be read back, only remembered.** NVML will not report one: `nvmlDeviceGetClock`
  answers NOT_SUPPORTED for every application-clock id, and the clocks-event reasons read zero
  whether a lock is set or not — both measured on a 5070 Ti with a lock deliberately applied. So
  that row shows what was asked for rather than what the card holds. The one thing that can make it
  a lie is a display driver reset, which clears the lock; the window notices that (the NVML session
  has to be reopened, which happens for no other reason) and forgets the range rather than reporting
  one nothing is holding.
- **Live MSVDD voltage is not readable.** Its ceiling and floor are set and read back, but the
  voltage it actually runs at is not, making it the one control here without read-back verification.
- **A display driver reset drops the tune, and nothing puts it back.** When a game hangs the card
  hard enough for Windows to reset the driver (`nvlddmkm` in the system event log), the voltage
  lock goes with it — the log shows the cap reading back as 0 mV afterwards — while the clock and
  memory offsets survive. Apply again to restore it. The app itself recovers: NVML sessions do not
  survive a reset, and one that does not is now thrown away and reopened rather than failing every
  call until the app is restarted.
- **Nothing guards an apply-at-logon that crashed the machine.** If a tune hangs the card on boot,
  the logon task will apply it again on the next one. Hold Shift during logon to skip it, or clear it
  with `startup --disable` from another account or safe mode.
- The AMD core-clock offset is a **ceiling**, not a shift. A power-limited card will ignore it —
  check the limiter line before concluding it's broken.
- Clock snapping applies to **offsets**. A card reporting an absolute memory clock (AMD) is left
  unsnapped, since rounding its stock clock would overclock it just from reading its state.
- RDNA 4 exposes no editable V/F curve and no temperature limit; both are hidden rather than faked.
- The V/F curve editor is NVIDIA-only. Multi-GPU is implemented but untested.
- vBIOS flashing is deliberately out of scope. The release binary is unsigned, so SmartScreen will
  warn on first run.

---

