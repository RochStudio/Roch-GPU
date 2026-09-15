namespace GpuTuner.Core.Backends.Nvidia;

// Verified against the reference tool's modern OCP call sites and live 616.92 read/write.
// These are full allocation sizes, NOT the low 16 bits of the version word.
internal static class ModernOcpControl
{
    internal const int ControlSize = 0x2486E0, ControlVersion = 0x2786E0;
    internal const int RangeSize = 0x2BA030;
    private static int Entry(int slot) => 0xA68 + slot * 0x2448;
    private static int Value(int slot) => Entry(slot) + 0x44;
    private static int Read(byte[] b, int at) => BitConverter.ToInt32(b, at);
    private static void Put(byte[] b, int at, int value) => BitConverter.GetBytes(value).CopyTo(b, at);

    private static byte[] NewControl()
    {
        var b = new byte[ControlSize];
        Put(b, 0, ControlVersion);
        Put(b, 0x88, 0x1FFFF);
        return b;
    }

    private static bool Valid(byte[] b) =>
        b.Length == ControlSize && Read(b, 0) == ControlVersion &&
        (Read(b, 0x88) & 0x6000) == 0x6000 &&
        Read(b, Entry(13)) == 19 && Read(b, Entry(14)) == 19 &&
        Read(b, Value(13)) > 0 && Read(b, Value(14)) > 0;

    internal static string? Apply(int slot, int requestedMa, Func<byte[], int> get, Func<byte[], int> info, Func<byte[], int> set)
    {
        if (slot is not (13 or 14) || requestedMa <= 0)
            return "Invalid OCP rail or nonpositive limit; no write attempted.";
        var control = NewControl();
        int status = get(control);
        if (status != 0) return $"Reading modern OCP control: status {status}; no write attempted.";
        if (!Valid(control)) return "Modern OCP layout/rail validation failed; no write attempted.";
        int current = Read(control, Value(slot));
        if (current == requestedMa) return null;
        int otherSlot = slot == 13 ? 14 : 13;
        int other = Read(control, Value(otherSlot));

        var ranges = new byte[RangeSize];
        Put(ranges, 0, RangeSize);
        status = info(ranges);
        if (status != 0) return $"Reading modern OCP bounds: status {status}; no write attempted.";
        int at = 0x4B0 + slot * 0x296C;
        if (Read(ranges, 0) != RangeSize || Read(ranges, at) != 19)
            return "Modern OCP range layout/rail validation failed; no write attempted.";
        int min = Read(ranges, at + 0xC), def = Read(ranges, at + 0x10), max = Read(ranges, at + 0x14);
        if (min <= 0 || min > def || def > max || current < min || current > max || requestedMa < min || requestedMa > max)
            return "OCP request or metadata is outside the driver-reported bounds; no write attempted.";

        // Preserve the full getter payload; modify only the selected output-current field.
        Put(control, Value(slot), requestedMa);
        status = set(control);
        if (status != 0) return $"nvapi 0xAFFC2279 (modern OCP layout): status {status}";
        var actual = NewControl();
        status = get(actual);
        if (status != 0 || !Valid(actual))
            return $"OCP write returned success but read-back could not be verified: status {status}";
        if (Read(actual, Value(slot)) != requestedMa || Read(actual, Value(otherSlot)) != other)
            return "OCP read-back mismatch: requested rail or untouched rail differs; recovery required.";
        return null;
    }
}
