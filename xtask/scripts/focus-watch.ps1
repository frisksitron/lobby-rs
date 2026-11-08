# Logs the foreground window (handle, process, title) and the cursor position every 100 ms for
# $Seconds, to prove a drive run takes neither the focus nor the mouse. Read-only: it only asks.
param([int]$Seconds = 60, [string]$Out = "fgmon.csv")
Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class FgMon {
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X; public int Y; }
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
}
"@
"time,hwnd,pid,process,title,cursor_x,cursor_y" | Out-File -Encoding utf8 $Out
$end = (Get-Date).AddSeconds($Seconds)
while ((Get-Date) -lt $end) {
  $h = [FgMon]::GetForegroundWindow()
  $procId = 0
  [void][FgMon]::GetWindowThreadProcessId($h, [ref]$procId)
  $name = (Get-Process -Id $procId -ErrorAction SilentlyContinue).ProcessName
  $sb = New-Object System.Text.StringBuilder 256
  [void][FgMon]::GetWindowText($h, $sb, 256)
  $p = New-Object FgMon+POINT
  [void][FgMon]::GetCursorPos([ref]$p)
  $title = $sb.ToString().Replace('"', "'")
  "$((Get-Date).ToString('HH:mm:ss.fff')),$h,$procId,$name,`"$title`",$($p.X),$($p.Y)" | Out-File -Append -Encoding utf8 $Out
  Start-Sleep -Milliseconds 100
}
