# Windows-side host audio player used by the emulator under WSL.
#
# WSLg routes Linux audio through its PulseAudio RDP sink, which was measured
# to fall progressively behind real time on long streams (see
# debugging/still_alive_no_audio_hang_and_choppy.md). This script bypasses it:
# the emulator runs it via powershell.exe interop and pipes raw mono s16le PCM
# to stdin, and it plays that PCM with the Win32 waveOut API.
#
# Protocol with the emulator (src/audio.rs):
# - stdin: raw PCM. EOF ends playback once queued audio drains.
# - stdout: one line "p <bytes>" each time a buffer of real (non-keep-alive)
#   PCM finishes playing, giving the running total. The emulator paces the
#   guest audio device by these reports, so it never runs more than a few
#   buffers ahead of what is actually audible.
# The emulator prepends `$Rate = <hz>` before running this script.
#
# Keep-alive: when stdin has no data and at most KEEPALIVE_PENDING buffers are
# still queued, a silent buffer is submitted (padded around any partial data).
# The Windows output stream therefore never drains between sounds; otherwise
# the device can go idle and clip the start of the next sound while it wakes.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$src = @"
using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

public static class DioptasePcmPlayer {
  [StructLayout(LayoutKind.Sequential)]
  struct WaveFormatEx {
    public ushort wFormatTag; public ushort nChannels; public uint nSamplesPerSec;
    public uint nAvgBytesPerSec; public ushort nBlockAlign; public ushort wBitsPerSample;
    public ushort cbSize;
  }

  [StructLayout(LayoutKind.Sequential)]
  struct WaveHdr {
    public IntPtr lpData; public uint dwBufferLength; public uint dwBytesRecorded;
    public IntPtr dwUser; public uint dwFlags; public uint dwLoops; public IntPtr lpNext;
    public IntPtr reserved;
  }

  const ushort WAVE_FORMAT_PCM = 1;
  const uint WHDR_DONE = 1;
  const uint CALLBACK_EVENT = 0x50000;
  const uint WAVE_MAPPER = 0xFFFFFFFF;
  // 40 ms per buffer, 6 buffers: at most 240 ms of queued Windows audio.
  const int BUFFER_MS = 40;
  const int BUFFER_COUNT = 6;
  // Submit keep-alive silence once this few buffers remain queued with no data.
  const int KEEPALIVE_PENDING = 2;

  [DllImport("winmm.dll")] static extern int waveOutOpen(out IntPtr h, uint dev, ref WaveFormatEx fmt, IntPtr cb, IntPtr inst, uint flags);
  [DllImport("winmm.dll")] static extern int waveOutPrepareHeader(IntPtr h, IntPtr hdr, int size);
  [DllImport("winmm.dll")] static extern int waveOutUnprepareHeader(IntPtr h, IntPtr hdr, int size);
  [DllImport("winmm.dll")] static extern int waveOutWrite(IntPtr h, IntPtr hdr, int size);
  [DllImport("winmm.dll")] static extern int waveOutClose(IntPtr h);

  // PCM read from stdin by the reader thread and not yet submitted. Guarded by gate.
  static readonly object gate = new object();
  static byte[] fifo = new byte[1 << 16];
  static int fifoHead, fifoCount;
  static bool inputEnded;

  static IntPtr[] hdrs;
  static bool[] submitted;
  static int[] dataBytes;        // real PCM bytes in each submitted buffer
  static long playedBytes;       // real PCM bytes whose buffers have finished
  static Stream progress;

  static uint Flags(IntPtr hdr) {
    return ((WaveHdr)Marshal.PtrToStructure(hdr, typeof(WaveHdr))).dwFlags;
  }

  static void Check(int rc, string op) {
    if (rc != 0) throw new Exception("dioptase audio player: " + op + " failed with MMRESULT " + rc);
  }

  // Copy stdin into the FIFO until EOF, growing it if the emulator runs ahead.
  static void ReaderLoop() {
    var input = Console.OpenStandardInput();
    var chunk = new byte[8192];
    while (true) {
      int n = input.Read(chunk, 0, chunk.Length);
      lock (gate) {
        if (n <= 0) { inputEnded = true; Monitor.PulseAll(gate); return; }
        if (fifoCount + n > fifo.Length) {
          var bigger = new byte[Math.Max(fifo.Length * 2, fifoCount + n)];
          for (int i = 0; i < fifoCount; i++) bigger[i] = fifo[(fifoHead + i) % fifo.Length];
          fifo = bigger; fifoHead = 0;
        }
        for (int i = 0; i < n; i++) fifo[(fifoHead + fifoCount + i) % fifo.Length] = chunk[i];
        fifoCount += n;
        Monitor.PulseAll(gate);
      }
    }
  }

  // Move up to max bytes (an even count) from the FIFO into dest. Caller holds gate.
  static int Take(byte[] dest, int max) {
    int n = Math.Min(fifoCount, max) & ~1;
    for (int i = 0; i < n; i++) dest[i] = fifo[(fifoHead + i) % fifo.Length];
    fifoHead = (fifoHead + n) % fifo.Length;
    fifoCount -= n;
    return n;
  }

  // Count finished buffers' real PCM as played and report the new total.
  static void CollectDone() {
    bool advanced = false;
    for (int i = 0; i < BUFFER_COUNT; i++) {
      if (submitted[i] && (Flags(hdrs[i]) & WHDR_DONE) != 0) {
        submitted[i] = false;
        if (dataBytes[i] > 0) { playedBytes += dataBytes[i]; advanced = true; }
      }
    }
    if (advanced) {
      byte[] line = Encoding.ASCII.GetBytes("p " + playedBytes + "\n");
      progress.Write(line, 0, line.Length);
      progress.Flush();
    }
  }

  static int PendingBuffers() {
    int pending = 0;
    for (int i = 0; i < BUFFER_COUNT; i++) if (submitted[i]) pending++;
    return pending;
  }

  public static void Run(int rate) {
    var fmt = new WaveFormatEx {
      wFormatTag = WAVE_FORMAT_PCM, nChannels = 1, nSamplesPerSec = (uint)rate,
      nAvgBytesPerSec = (uint)(rate * 2), nBlockAlign = 2, wBitsPerSample = 16, cbSize = 0
    };
    var ev = new AutoResetEvent(false);
    IntPtr wave;
    Check(waveOutOpen(out wave, WAVE_MAPPER, ref fmt,
      ev.SafeWaitHandle.DangerousGetHandle(), IntPtr.Zero, CALLBACK_EVENT), "waveOutOpen");

    int bytes = (rate * 2 * BUFFER_MS / 1000) & ~1;
    int hdrSize = Marshal.SizeOf(typeof(WaveHdr));
    hdrs = new IntPtr[BUFFER_COUNT];
    submitted = new bool[BUFFER_COUNT];
    dataBytes = new int[BUFFER_COUNT];
    for (int i = 0; i < BUFFER_COUNT; i++) {
      // Headers live in unmanaged memory because the driver updates dwFlags.
      hdrs[i] = Marshal.AllocHGlobal(hdrSize);
      var hdr = new WaveHdr { lpData = Marshal.AllocHGlobal(bytes), dwBufferLength = (uint)bytes };
      Marshal.StructureToPtr(hdr, hdrs[i], false);
      Check(waveOutPrepareHeader(wave, hdrs[i], hdrSize), "waveOutPrepareHeader");
    }

    progress = Console.OpenStandardOutput();
    var reader = new Thread(ReaderLoop);
    reader.IsBackground = true;
    reader.Start();

    var chunk = new byte[bytes];
    int slot = 0;
    while (true) {
      // Wait for this slot's previous buffer to finish.
      while (submitted[slot]) {
        CollectDone();
        if (submitted[slot]) ev.WaitOne(10);
      }

      // Fill the slot with real PCM if a whole buffer is ready; at EOF or when
      // the queue is about to drain, take whatever is there and pad with silence.
      int got;
      bool ended;
      lock (gate) {
        while (true) {
          ended = inputEnded;
          if (fifoCount >= bytes || ended) { got = Take(chunk, bytes); break; }
          CollectDone();
          if (PendingBuffers() <= KEEPALIVE_PENDING) { got = Take(chunk, bytes); break; }
          Monitor.Wait(gate, 5);
        }
      }
      if (ended && got == 0) break;

      Array.Clear(chunk, got, bytes - got);
      var hdr = (WaveHdr)Marshal.PtrToStructure(hdrs[slot], typeof(WaveHdr));
      Marshal.Copy(chunk, 0, hdr.lpData, bytes);
      hdr.dwBufferLength = (uint)bytes;
      hdr.dwFlags &= ~WHDR_DONE;
      Marshal.StructureToPtr(hdr, hdrs[slot], false);
      dataBytes[slot] = got;
      submitted[slot] = true;
      Check(waveOutWrite(wave, hdrs[slot], hdrSize), "waveOutWrite");
      slot = (slot + 1) % BUFFER_COUNT;
    }

    while (PendingBuffers() > 0) {
      CollectDone();
      ev.WaitOne(10);
    }
    for (int i = 0; i < BUFFER_COUNT; i++) waveOutUnprepareHeader(wave, hdrs[i], hdrSize);
    waveOutClose(wave);
  }
}
"@

Add-Type -TypeDefinition $src
[DioptasePcmPlayer]::Run($Rate)
