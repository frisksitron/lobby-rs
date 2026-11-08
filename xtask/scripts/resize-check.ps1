param(
    [Parameter(Mandatory)][int]$ProcessId,
    [string]$OutDir = ".",
    [string]$Sizes = "1400x900,700x500,1900x1100,960x700,2400x1300,1360x960",
    [switch]$Maximize,
    [int]$Stress = 0
)
Set-Variable -Name ResizeStressIntervalMs -Value 25 -Option Constant
Set-Variable -Name ResizeSettleIntervalMs -Value 700 -Option Constant
# Resizes a window without activating it (SetWindowPos, SWP_NOACTIVATE) through a list of physical
# sizes, then optionally maximizes and restores it (ShowWindow; this one activates, so the
# previous foreground window is given the focus back at once). After each change it captures the
# window with PrintWindow(PW_RENDERFULLCONTENT) and reports how much of the client area is black
# (the bug: only the old area drawn, the rest black).
Add-Type -AssemblyName System.Drawing
if (-not ('R' -as [type])) {
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class R {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int hh, uint flags);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
}
'@
}
[R]::SetProcessDPIAware() | Out-Null
$h = (Get-Process -Id $ProcessId).MainWindowHandle
if ($h -eq 0) { throw "no main window" }

function Capture([string]$name) {
    Start-Sleep -Milliseconds $ResizeSettleIntervalMs
    $r = New-Object R+RECT; [R]::GetWindowRect($h, [ref]$r) | Out-Null
    $c = New-Object R+RECT; [R]::GetClientRect($h, [ref]$c) | Out-Null
    $w = $r.R - $r.L; $hh = $r.B - $r.T
    $bmp = New-Object System.Drawing.Bitmap $w, $hh
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $dc = $g.GetHdc(); [R]::PrintWindow($h, $dc, 2) | Out-Null; $g.ReleaseHdc($dc); $g.Dispose()
    $path = Join-Path $OutDir "$name.png"
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    # Black pixels on a sparse grid of the whole capture.
    $black = 0; $total = 0
    for ($y = 2; $y -lt $hh - 2; $y += 6) { for ($x = 2; $x -lt $w - 2; $x += 6) {
        $p = $bmp.GetPixel($x, $y); $total++
        if ($p.R -lt 10 -and $p.G -lt 10 -and $p.B -lt 10) { $black++ }
    } }
    $bmp.Dispose()
    "{0}: window {1}x{2}, client {3}x{4}, black {5:P1}" -f $name, $w, $hh, $c.R, $c.B, ($black / [math]::Max(1, $total))
}

Capture "00-start"
# Rapid alternating resizes, 25 ms apart (the "repeated resizes" half of the bug).
for ($k = 0; $k -lt $Stress; $k++) {
    $w = if ($k % 2) { 700 } else { 2600 }; $hh = if ($k % 2) { 500 } else { 1500 }
    [R]::SetWindowPos($h, [IntPtr]::Zero, 0, 0, $w, $hh, 0x0016) | Out-Null
    Start-Sleep -Milliseconds $ResizeStressIntervalMs
}
if ($Stress -gt 0) { Capture "00-after-stress" }
$i = 1
foreach ($s in $Sizes.Split(',')) {
    $wh = $s.Split('x'); $w = [int]$wh[0]; $hh = [int]$wh[1]
    # SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE
    [R]::SetWindowPos($h, [IntPtr]::Zero, 0, 0, $w, $hh, 0x0002 -bor 0x0004 -bor 0x0010) | Out-Null
    Capture ("{0:D2}-{1}" -f $i, $s)
    $i++
}
if ($Maximize) {
    $prev = [R]::GetForegroundWindow()
    [R]::ShowWindow($h, 3) | Out-Null  # SW_MAXIMIZE
    if ($prev -ne $h) { [R]::SetForegroundWindow($prev) | Out-Null }
    Capture ("{0:D2}-maximized" -f $i); $i++
    [R]::ShowWindow($h, 4) | Out-Null  # SW_SHOWNOACTIVATE: restore without activating
    Capture ("{0:D2}-restored" -f $i); $i++
    [R]::ShowWindow($h, 3) | Out-Null
    if ($prev -ne $h) { [R]::SetForegroundWindow($prev) | Out-Null }
    Capture ("{0:D2}-maximized-again" -f $i); $i++
    [R]::ShowWindow($h, 4) | Out-Null
    Capture ("{0:D2}-restored-again" -f $i)
}
