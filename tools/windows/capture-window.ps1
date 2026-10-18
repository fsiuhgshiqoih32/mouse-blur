param([string]$Title = 'Mouse Blur', [string]$Out, [int]$Count = 1, [int]$DelayMs = 300)
# Captures a window by title with PrintWindow (works for OpenGL windows such as the egui one).
#   .\tools\windows\capture-window.ps1 -Out shot.png [-Count 3 -DelayMs 300]
Add-Type -AssemblyName System.Drawing
if (-not ('WinCapTool' -as [type])) {
Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @'
using System; using System.Runtime.InteropServices; using System.Drawing;
public static class WinCapTool {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] static extern bool SetProcessDpiAwarenessContext(IntPtr v);
  public static void Save(IntPtr h, string path) {
    SetProcessDpiAwarenessContext(new IntPtr(-4));
    RECT r; GetWindowRect(h, out r);
    using (var bmp = new Bitmap(r.R - r.L, r.B - r.T)) {
      using (var g = Graphics.FromImage(bmp)) { IntPtr dc = g.GetHdc(); PrintWindow(h, dc, 2); g.ReleaseHdc(dc); }
      bmp.Save(path);
    }
  }
}
'@
}
$w = Get-Process | Where-Object { $_.MainWindowTitle -eq $Title } | Select-Object -First 1
if (-not $w) { Write-Output "no window titled '$Title'"; exit 1 }
for ($i = 1; $i -le $Count; $i++) {
  $path = if ($Count -eq 1) { $Out } else { $Out -replace '\.png$', "-$i.png" }
  [WinCapTool]::Save($w.MainWindowHandle, $path)
  Write-Output $path
  Start-Sleep -Milliseconds $DelayMs
}
