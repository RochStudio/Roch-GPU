# AMD probes

Read-only PowerShell scripts used to map the AMD driver surface while writing the ADL backend. They
are kept because they are the fastest way to find out what a *different* AMD card exposes, and
because they document how the backend's assumptions were established.

Run any of them from PowerShell:

```powershell
iex (Get-Content .\AmdProbe.ps1 -Raw)
```

| Script | What it answers |
|---|---|
| `AmdProbe.ps1`  | Which libraries are present, what Overdrive version the driver reports, which PMLog sensors exist |
| `AmdProbe2.ps1` | Every `ADL*Overdrive*` entry point the installed `atiadlxx.dll` actually exports, across all adapter indices |
| `AmdProbe3.ps1` | The full Overdrive 8 feature table: capability bits, and each feature's min/max/default/current |
| `AmdProbe4.ps1` | Whether a core-clock offset write actually sticks, tried in four different call shapes |

`AmdProbe4.ps1` is the only one that writes anything: it sets the core clock offset, reads it back,
and restores it to 0. The others touch nothing.
