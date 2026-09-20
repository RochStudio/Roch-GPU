namespace GpuTuner.Core.Backends.Nvidia;

internal static class DomainOffsetControl
{
    internal const int Size = 0x61A4, Version = 0x261A4;
    internal const int InvalidRequest = -10001, InvalidLayout = -10002, VerificationFailed = -10003;

    // The leading block word identifies the layout, not a domain count.
    // Verified layout 10 (Ada) uses +0x10C; layout 15 uses +0x114.
    internal static int OffsetField(byte[] control, int slot)
    {
        if (slot < 0 || slot >= 32 || control.Length != Size) return -1;
        int block = 0x124 + slot * 0x304;
        if (block + 0x118 > control.Length) return -1;
        return BitConverter.ToInt32(control, block) switch
        {
            10 => block + 0x10C,
            15 => block + 0x114,
            _ => -1
        };
    }
    internal static int Apply(int slot, int mhz, Func<byte[], int> get, Func<byte[], int> set)
    {
        if (slot < 0 || slot >= 32 || mhz < int.MinValue / 1000 || mhz > int.MaxValue / 1000)
            return InvalidRequest;
        int field;
        int mask = 1 << slot;
        byte[] NewRequest()
        {
            var b = new byte[Size];
            Put(b, 0, Version);
            Put(b, 8, mask);
            return b;
        }
        bool Valid(byte[] b) => BitConverter.ToInt32(b, 0) == Version &&
            (BitConverter.ToInt32(b, 8) & mask) == mask;

        var control = NewRequest();
        int status = get(control);
        if (status != 0) return status;
        if (!Valid(control)) return InvalidLayout;
        field = OffsetField(control, slot);
        if (field < 0) return InvalidLayout;
        int requested = mhz * 1000;
        if (BitConverter.ToInt32(control, field) == requested) return 0;
        // Preserve the getter payload, selecting only the requested domain.
        Put(control, 8, mask);
        Put(control, field, requested);
        status = set(control);
        if (status != 0) return status;
        var verify = NewRequest();
        status = get(verify);
        if (status != 0) return status;
        if (!Valid(verify) || OffsetField(verify, slot) != field) return InvalidLayout;
        return BitConverter.ToInt32(verify, field) == requested ? 0 : VerificationFailed;
    }

    private static void Put(byte[] b, int at, int value) => BitConverter.GetBytes(value).CopyTo(b, at);
}
