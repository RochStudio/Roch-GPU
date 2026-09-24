# B60 clock-range integration validation

Tested on 2026-09-23 (local time), Intel Arc Pro B60 / driver 32.0.101.9030.

The test used the real MainViewModel, ProfileStore in an isolated test directory, TuningService.Apply and Intel backend. User profile slots and startup configuration were not edited.

- Initial values: core offset +90 MHz, memory 19000 Mbps, voltage limit 50%, power 120%, temperature 100%, core range 400–2400 MHz, existing six-point hardware fan curve.
- Saved and loaded a test profile with a 500–2300 MHz range; enabled state and both endpoints persisted.
- Applied both endpoints through the main profile path and verified readback after 1.2 seconds.
- Reduced power to 115%; verified both endpoints and the exact fan curve stayed intact.
- Disabled the range through the same profile path; factory readback was 400–2400 MHz.
- Restored the original profile in a finally block; after a further second, every field of the full tuning state matched the original snapshot, including power, voltage, memory, core offset and hardware fan points.

The successful run passed 14 integration assertions. Four further checks against recorded evidence verified unchanged V/F frequencies for all three test stages and exact complete before/after tuning-state equality. Earlier automated regression coverage remains 518 passing checks; production code was unchanged during this investigation.

An initial overly strict assertion compared all V/F voltage coordinates and failed while the narrower range was active. Restoration passed. Inspection of all ten points showed a uniform change in voltage coordinates with no change in stock/live frequencies. This is consistent with the backend's already documented operating-condition-dependent Intel voltage grid. The subsequent test records these readings and checks the programmed frequencies separately. At the end of the successful run the complete curve, including voltage coordinates, matched its initial snapshot too.

Evidence: ignored local directory `test-results/intel/range-integration/run-20260924-012849`, containing before/after snapshots and three intermediate curve snapshots. The test harness is in the adjacent `Program.cs`.

This verifies settings application, persistence, interactions and restoration. It does not establish sustained clocks under load or gaming/stress-test stability.
