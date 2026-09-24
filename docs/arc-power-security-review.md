# Arc Power source review — 2026-09-23

Reviewed repository: https://github.com/YamsSE/Arc-Power at commit `9366e65bc6fe9db9187c8aa9f9d9a196bf3bdb31` (package version 1.1.7). This is a scoped static review, dependency advisory lookup and local signature inspection, not a malware certification, complete audit, or validation of downloadable release executables. No Arc Power application, installer, dependency script or bundled binary was executed.

## Assessment

Suitable as a reference for understanding available APIs, with independent verification. I would not describe the downloaded app as verified safe to run as administrator based on this review. Roch GPU's new clock-range feature is independently implemented against the installed Intel driver; no Arc Power source or binaries were imported.

## Findings

1. **Update executable authenticity is not verified in the reviewed handoff path.** `src/main/auto-update-runtime.js` downloads into a predictable temporary directory and later launches the expected executable or a replacement script. URL/path validation restricts GitHub repository/filename, but I found no cryptographic digest or publisher-signature verification in this path before execution. Because packaged Arc Power requests administrator privileges, replacing the file between download and execution or compromising a trusted release source has a significant impact. This is a code-review finding, not a demonstrated exploit.
2. **Known dependency advisories.** The npm bulk advisory service returned affected ranges matching locked versions in five packages: Electron 37.10.3, @xmldom/xmldom 0.8.13, extract-zip 2.0.1, fast-uri 3.1.5 and js-yaml 4.3.1. These entries are marked development dependencies in the lockfile; Electron nevertheless supplies the shipped runtime. Several Electron advisories are platform-specific or require features not established as reachable here. Build-tool findings do not automatically mean the installed application is exploitable. Raw results are in the ignored local `test-results/arc-power-advisories.json`.
3. **Privileged bundled components require additional trust.** CPU telemetry's `msr-reader.js` invokes bundled PawnIO setup silently when the driver is absent. Local Authenticode inspection reports that installer Valid (publisher namazso). The bundled `backend/igcl2023/IntelControlLib.dll` reports NotSigned; that is not proof of malware, but its authenticity was not established in this review. Merely reading source does not verify every packaged binary.

## Positive controls observed

Renderer windows use context isolation, sandboxing and disabled Node integration; the main HTML has a restrictive content security policy. Update URLs are restricted to the project's GitHub release path, paths are validated before handoff, and elevated worker requests/results use HMAC authentication. The package has a lockfile. These are useful controls but do not eliminate the findings above.

## Scope limits and references

Reviewed package metadata/lockfile, primary renderer configuration, IPC registration, updater validation/download/handoff, elevated-worker authentication, and CPU telemetry's driver-install path. I did not execute repository tests, inspect every source line, reverse-engineer binaries, establish release reproducibility, or test exploitability. The GPL-2.0-only license would also need consideration before copying code; this implementation copies none.

- [Reviewed source](https://github.com/YamsSE/Arc-Power/tree/9366e65bc6fe9db9187c8aa9f9d9a196bf3bdb31)
- [Update runtime](https://github.com/YamsSE/Arc-Power/blob/9366e65bc6fe9db9187c8aa9f9d9a196bf3bdb31/src/main/auto-update-runtime.js)
- [CPU telemetry / PawnIO](https://github.com/YamsSE/Arc-Power/blob/9366e65bc6fe9db9187c8aa9f9d9a196bf3bdb31/src/main/msr-reader.js)
- [Dependency lockfile](https://github.com/YamsSE/Arc-Power/blob/9366e65bc6fe9db9187c8aa9f9d9a196bf3bdb31/package-lock.json)
- [Electron IPC advisory](https://github.com/advisories/GHSA-xj5x-m3f3-5x3h)
- [extract-zip advisory](https://github.com/advisories/GHSA-7pqw-9j4j-h8q3)
