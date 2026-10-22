LagScope - finds out where lag in online games comes from
=========================================================

Start
-----
Double-click lagscope.exe.
* "Do you want to allow this app to make changes?" -> Yes
  (it needs administrator rights to measure network and driver timing).
* "Windows protected your PC" -> More info -> Run anyway
  (the program is not code-signed; that is all this warning means).


Record (the main thing)
-----------------------
1. Record tab: pick your game (or Auto-detect) and click Start recording.
2. Play as usual. Press F8 whenever it feels laggy (you hear a beep).
3. Click Stop & build report, then Open report.

The coloured line at the top is the verdict. Under it, one dot per area
(Wi-Fi, internet, frames, CPU, GPU, memory, disk, drivers, display & power,
hardware setup):
green is fine, amber is worth a look. "What to try" lists the next steps.

Tip: tick "Record automatically when I play ..." and LagScope waits in the tray
(next to the clock) and makes a report after every game. Closing the window keeps
it there; right-click the tray icon to exit.


Other tabs
----------
Wi-Fi fix    For freezes that happen even at the desktop. Takes ~15 minutes with
             games closed. It switches suspect Wi-Fi settings off one at a time for
             a minute and puts each back. Nothing changes without your Yes, and a
             kept change can be undone on the same tab.
Network      Your router and provider; a "busy connection" test (does ping jump
             while the line is full? uses up to 2 GB of data); and how crowded
             your Wi-Fi channel is.
Advanced     (bottom of the Record tab) turn off driver & disk tracing, e.g. when
             LatencyMon or a similar tool needs the Windows kernel trace.
Reports      Everything you recorded or tested. Tick two and click Compare to see
             what changed, e.g. before and after a fix.


Sharing a report
----------------
Each recording also makes a LagScope_....zip (Reports -> More -> Show files).
Send that zip to whoever is helping you.


Safe and private
----------------
* Only measures; it never touches the game (no injection, no memory reading),
  so it is safe with anti-cheat such as Easy Anti-Cheat.
* Light: a few percent of one CPU core while recording, nothing when idle.
* Only the busy-connection test sends real traffic (to Cloudflare's speed test),
  and only when you click it.
* The zip holds timing, ping, hardware and settings data and process names.
  Wi-Fi names, MAC addresses, the PC name and the user name are removed.

If something goes wrong
* The window does not open: right-click lagscope.exe -> Run as administrator.
* The PC lost power during a Wi-Fi fix: start LagScope again; it puts back any
  setting that was switched for a test.
* F8 does something in your game: pick another mark key on the Record tab.


Command line (optional; lagscope.exe --help lists everything)
  lagscope.exe --console          text menu instead of the window
  lagscope.exe --record --game cs2 --hotkey F9 --duration 1800
  lagscope.exe --wifi-check       the Wi-Fi fix in a console window
  lagscope.exe --undo             undo the settings LagScope kept
  lagscope.exe games              list the game profiles
  lagscope.exe compare <A> <B>    compare two report folders
  lagscope.exe load-test          busy-connection (bufferbloat) test
  lagscope.exe wifi-scan          nearby Wi-Fi
  lagscope.exe router             router / provider identification
  lagscope.exe startup on|off     start hidden in the tray with Windows
