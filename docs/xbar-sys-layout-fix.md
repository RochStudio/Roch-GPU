# XBAR / SYS layout correction

The RTX 4070 / 591.86 rejection was an application layout bug, not proof that
the hardware lacked clock-offset control. The temporary model/driver restriction
has been removed.

The user supplied mVolt+.exe (SHA-256
`62D69815434BC52A8DF76B44DD54A69DF34D66154EDAF598B02128273F330416`). Read-only
binary inspection of its layout selection and setter at image addresses
0x140112057 and 0x140112F2F showed that the shared outer control version does
not imply a fixed inner offset field. The independently implemented decoder uses:

| Block tag | Offset within block |
| --- | --- |
| 10 | 0x10C |
| 15 | 0x114 |

Blocks start at 0x124 + slot * 0x304 in the 0x61A4-byte version-2 control.
XBAR uses slot 1; SYS uses slot 3. Both reads and writes select their field
from the returned block tag. Unknown layouts prevent writes. Successful writes
require exact offset readback; an idle frequency counter is not an offset.

On 2026-09-20, the corrected reader found the user's existing +15 MHz XBAR
and SYS offsets, where the old reader reported zero. Each domain then passed
+15 -> 0 -> +15 MHz with status 0 and exact readback. The other domain's
offset remained +15 throughout. Both original +15 MHz offsets were restored.
No other tuning controls were changed by these domain tests. This is not a
stress-stability or performance test.

Validation: clean Release build, 425 core checks and 17 telemetry checks passed.
Regression cases cover both layouts, XBAR/SYS selection, preserving every byte
outside the selected offset, unknown layouts, failed getters, rejected setters,
ignored writes, unchanged-value no-ops, and offset restoration.

Local test evidence is under `test-results/mvolt-*`. The reference executable
is not included or linked into Roch GPU.
