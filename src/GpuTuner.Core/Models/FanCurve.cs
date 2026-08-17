using System.Text.Json.Serialization;

namespace GpuTuner.Core.Models;

/// <summary>A single (temperature °C, fan %) point.</summary>
public record struct FanPoint(double TemperatureC, double FanPercent);

/// <summary>
/// Piecewise-linear temperature→fan% curve with hysteresis and a minimum step,
/// mirroring what Afterburner's "user defined" curve does.
/// </summary>
public sealed class FanCurve
{
    public List<FanPoint> Points { get; set; } = DefaultPoints();

    /// <summary>Degrees the temperature must fall below the last trigger before the fan is allowed to slow down.</summary>
    public double HysteresisC { get; set; } = 3.0;

    /// <summary>Ignore fan changes smaller than this (%) to avoid constant tiny adjustments.</summary>
    public double MinimumStepPercent { get; set; } = 2.0;

    /// <summary>Below this temperature the fan is allowed to stop entirely (if the card supports 0%).</summary>
    public bool ZeroRpmBelowFirstPoint { get; set; } = false;

    [JsonIgnore] private double _lastTemp = double.NaN;
    [JsonIgnore] private double _lastOutput = double.NaN;

    public static List<FanPoint> DefaultPoints() => new()
    {
        new(30, 30), new(50, 40), new(60, 55), new(70, 70), new(80, 90), new(90, 100)
    };

    /// <summary>Pure interpolation without hysteresis. Public so the UI can draw the curve.</summary>
    public double Evaluate(double temperatureC)
    {
        var pts = Points.OrderBy(p => p.TemperatureC).ToList();
        if (pts.Count == 0) return 50;
        if (temperatureC <= pts[0].TemperatureC)
            return ZeroRpmBelowFirstPoint && temperatureC < pts[0].TemperatureC ? 0 : pts[0].FanPercent;
        if (temperatureC >= pts[^1].TemperatureC) return pts[^1].FanPercent;

        for (int i = 0; i < pts.Count - 1; i++)
        {
            var a = pts[i]; var b = pts[i + 1];
            if (temperatureC >= a.TemperatureC && temperatureC <= b.TemperatureC)
            {
                if (Math.Abs(b.TemperatureC - a.TemperatureC) < 1e-9) return b.FanPercent;
                var t = (temperatureC - a.TemperatureC) / (b.TemperatureC - a.TemperatureC);
                return a.FanPercent + t * (b.FanPercent - a.FanPercent);
            }
        }
        return pts[^1].FanPercent;
    }

    /// <summary>
    /// Stateful evaluation applying hysteresis and minimum step. Returns null when no change should be sent to the GPU.
    /// </summary>
    public double? Step(double temperatureC)
    {
        var target = Evaluate(temperatureC);

        if (double.IsNaN(_lastOutput))
        {
            _lastTemp = temperatureC; _lastOutput = target;
            return target;
        }

        // Rising: always follow immediately. Falling: only after hysteresis band cleared.
        bool falling = target < _lastOutput;
        if (falling && temperatureC > _lastTemp - HysteresisC)
            return null;

        if (Math.Abs(target - _lastOutput) < MinimumStepPercent)
            return null;

        _lastTemp = temperatureC; _lastOutput = target;
        return target;
    }

    public void ResetState() { _lastTemp = double.NaN; _lastOutput = double.NaN; }

    public FanCurve Clone() => new()
    {
        Points = Points.Select(p => p).ToList(),
        HysteresisC = HysteresisC,
        MinimumStepPercent = MinimumStepPercent,
        ZeroRpmBelowFirstPoint = ZeroRpmBelowFirstPoint
    };

    /// <summary>Clamp and sort points so the curve is monotonic in temperature and within 0..100%.</summary>
    public void Normalize()
    {
        Points = Points
            .Select(p => new FanPoint(Math.Clamp(p.TemperatureC, 0, 110), Math.Clamp(p.FanPercent, 0, 100)))
            .OrderBy(p => p.TemperatureC)
            .ToList();
    }
}
