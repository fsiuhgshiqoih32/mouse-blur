# Checks how DWM blends a per-pixel-alpha layered window on each monitor (e.g. with HDR on).
# Shows an opaque black and an opaque white square, puts a layered window with 25% coverage over
# each, captures the screen, and prints the composited values.
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type -ReferencedAssemblies System.Windows.Forms, System.Drawing -TypeDefinition @'
using System; using System.Drawing; using System.Runtime.InteropServices; using System.Windows.Forms;
public static class Blend {
  [StructLayout(LayoutKind.Sequential)] struct POINT { public int X, Y; }
  [StructLayout(LayoutKind.Sequential)] struct SIZE { public int CX, CY; }
  [StructLayout(LayoutKind.Sequential, Pack = 1)] struct BF { public byte Op, Flags, Alpha, Format; }
  [StructLayout(LayoutKind.Sequential)] struct BIH { public int Size, W, H; public short Planes, Bits; public int Comp, ImgSize, X, Y, Used, Imp; }
  [DllImport("user32.dll")] static extern bool UpdateLayeredWindow(IntPtr h, IntPtr dst, ref POINT p, ref SIZE s, IntPtr src, ref POINT sp, int key, ref BF bf, int flags);
  [DllImport("user32.dll")] static extern IntPtr GetDC(IntPtr h);
  [DllImport("user32.dll")] static extern int ReleaseDC(IntPtr h, IntPtr dc);
  [DllImport("user32.dll")] static extern bool SetProcessDpiAwarenessContext(IntPtr v);
  [DllImport("gdi32.dll")] static extern IntPtr CreateCompatibleDC(IntPtr dc);
  [DllImport("gdi32.dll")] static extern IntPtr CreateDIBSection(IntPtr dc, ref BIH bi, int u, out IntPtr bits, IntPtr s, int o);
  [DllImport("gdi32.dll")] static extern IntPtr SelectObject(IntPtr dc, IntPtr o);
  [DllImport("gdi32.dll")] static extern bool BitBlt(IntPtr d, int x, int y, int w, int h, IntPtr s, int sx, int sy, int rop);
  class Layered : Form {
    protected override CreateParams CreateParams { get { var cp = base.CreateParams; cp.ExStyle |= 0x80000 | 0x20 | 0x8 | 0x80; return cp; } }
  }
  static void Paint(Form f, int x, int y, int n, uint premult) {
    IntPtr screen = GetDC(IntPtr.Zero), mem = CreateCompatibleDC(screen); IntPtr bits;
    var bi = new BIH { Size = 40, W = n, H = -n, Planes = 1, Bits = 32 };
    IntPtr dib = CreateDIBSection(mem, ref bi, 0, out bits, IntPtr.Zero, 0); SelectObject(mem, dib);
    var px = new int[n * n]; for (int i = 0; i < px.Length; i++) px[i] = (int)premult; Marshal.Copy(px, 0, bits, px.Length);
    var p = new POINT { X = x, Y = y }; var s = new SIZE { CX = n, CY = n }; var z = new POINT(); var bf = new BF { Alpha = 255, Format = 1 };
    UpdateLayeredWindow(f.Handle, screen, ref p, ref s, mem, ref z, 0, ref bf, 2); ReleaseDC(IntPtr.Zero, screen);
  }
  static int Sample(int x, int y) {
    using (var bmp = new Bitmap(1, 1)) { using (var g = Graphics.FromImage(bmp)) { IntPtr d = g.GetHdc(); IntPtr s = GetDC(IntPtr.Zero); BitBlt(d, 0, 0, 1, 1, s, x, y, 0x00CC0020 | 0x40000000); ReleaseDC(IntPtr.Zero, s); g.ReleaseHdc(d); } return bmp.GetPixel(0, 0).R; }
  }
  public static string Run() {
    SetProcessDpiAwarenessContext(new IntPtr(-4));
    string result = "";
    foreach (var screen in Screen.AllScreens) {
      var b = screen.Bounds; int n = 120, x0 = b.X + b.Width / 2 - n - 20, x1 = b.X + b.Width / 2 + 20, y = b.Y + b.Height / 2 - n / 2;
      var black = new Form { FormBorderStyle = FormBorderStyle.None, BackColor = Color.Black, StartPosition = FormStartPosition.Manual, Bounds = new Rectangle(x0, y, n, n), TopMost = true, ShowInTaskbar = false };
      var white = new Form { FormBorderStyle = FormBorderStyle.None, BackColor = Color.White, StartPosition = FormStartPosition.Manual, Bounds = new Rectangle(x1, y, n, n), TopMost = true, ShowInTaskbar = false };
      black.Show(); white.Show();
      var over1 = new Layered { ShowInTaskbar = false }; var over2 = new Layered { ShowInTaskbar = false };
      over1.Show(); over2.Show();
      Paint(over1, x0, y, n, 0x40404040);   // 25% white, premultiplied
      Paint(over2, x1, y, n, 0x40000000);   // 25% black
      Application.DoEvents(); System.Threading.Thread.Sleep(400); Application.DoEvents();
      int onBlack = Sample(x0 + n / 2, y + n / 2), onWhite = Sample(x1 + n / 2, y + n / 2);
      result += string.Format("{0} ({1}x{2}): 25% white over black -> {3} (gamma blend 64, linear 137); 25% black over white -> {4} (gamma 191, linear 225)\n", screen.DeviceName, b.Width, b.Height, onBlack, onWhite);
      over1.Close(); over2.Close(); black.Close(); white.Close();
    }
    return result;
  }
}
'@
[Blend]::Run()
