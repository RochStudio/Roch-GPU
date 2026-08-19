using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;

namespace GpuTuner.App.Controls;

/// <summary>
/// Makes the numeric boxes behave the way every other tuning tool's do: one click selects the whole
/// number so typing replaces it, and Enter commits without having to Tab or click away.
///
/// Attached rather than baked into a control so the plain <see cref="TextBox"/>es keep their style —
/// the boxes are spread across three windows and only differ by which value they bind to.
/// </summary>
public static class ValueBoxBehavior
{
    public static readonly DependencyProperty SelectOnFocusProperty =
        DependencyProperty.RegisterAttached("SelectOnFocus", typeof(bool), typeof(ValueBoxBehavior),
            new PropertyMetadata(false, OnSelectOnFocusChanged));

    public static bool GetSelectOnFocus(DependencyObject o) => (bool)o.GetValue(SelectOnFocusProperty);
    public static void SetSelectOnFocus(DependencyObject o, bool value) => o.SetValue(SelectOnFocusProperty, value);

    private static void OnSelectOnFocusChanged(DependencyObject o, DependencyPropertyChangedEventArgs e)
    {
        if (o is not TextBox tb) return;

        tb.GotKeyboardFocus -= OnGotKeyboardFocus;
        tb.PreviewMouseLeftButtonDown -= OnPreviewMouseLeftButtonDown;
        tb.PreviewKeyDown -= OnPreviewKeyDown;

        if (!(bool)e.NewValue) return;
        tb.GotKeyboardFocus += OnGotKeyboardFocus;
        tb.PreviewMouseLeftButtonDown += OnPreviewMouseLeftButtonDown;
        tb.PreviewKeyDown += OnPreviewKeyDown;
    }

    private static void OnGotKeyboardFocus(object sender, KeyboardFocusChangedEventArgs e) => ((TextBox)sender).SelectAll();

    private static void OnPreviewMouseLeftButtonDown(object sender, MouseButtonEventArgs e)
    {
        var tb = (TextBox)sender;
        // Every click, not just the first one into an unfocused box. Letting a second click place a
        // caret is the usual Windows behaviour, but these boxes hold three or four right-aligned
        // digits: clicking left of the text puts the caret at position 0, so typing -45 over a 0
        // silently yields -450. Retyping the number is the only edit worth making here anyway.
        e.Handled = true;
        if (tb.IsKeyboardFocusWithin) tb.SelectAll();   // already focused, so GotKeyboardFocus won't fire
        else tb.Focus();
    }

    private static void OnPreviewKeyDown(object sender, KeyEventArgs e)
    {
        if (e.Key != Key.Enter) return;
        var tb = (TextBox)sender;
        var binding = tb.GetBindingExpression(TextBox.TextProperty);
        // These boxes commit on LostFocus, so push the typed text through by hand, then pull back
        // what the view model actually accepted — a value outside the card's range comes back
        // clamped, and the box should show that rather than the number that was refused.
        binding?.UpdateSource();
        binding?.UpdateTarget();
        tb.SelectAll();
        e.Handled = true;
    }
}
