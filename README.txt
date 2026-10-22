LagScope - finds (and fixes) where lag in online games comes from
=================================================================

Getting started
---------------
* Unzip LagScope.zip anywhere (e.g. the Desktop) and double-click lagscope.exe.
    - Windows asks "Do you want to allow this app to make changes?": click Yes
      (it needs administrator rights to measure network, Wi-Fi and driver details).
    - If a blue "Windows protected your PC" box appears: click "More info",
      then "Run anyway" (the program is not code-signed, that is all).
* The LagScope window has four tabs: Record, Wi-Fi check, Network and Sessions.


A. Wi-Fi check & fix  (if you play on Wi-Fi - about 15 minutes)
----------------------------------------------------------------
1. Close your games, and pause anything that downloads or streams (Steam
   downloads, OneDrive, videos). Discord can stay open.
2. Open the "Wi-Fi check" tab and click "Start Wi-Fi check".
3. LagScope asks a few questions in a small box:
      "Check Windows Update for newer Wi-Fi / Bluetooth drivers?"   -> Yes
      "Install these now?" (only if it found drivers)               -> Yes
         (if it then says "Restart the PC now": restart, and start the check again)
      "Allow these temporary changes?"                               -> Yes
4. Leave the PC alone until it finishes. Each setting is switched for one minute
   and then put back exactly as it was. What you will notice:
      - Wi-Fi drops for a few seconds a few times (when an adapter setting changes)
      - Bluetooth headset / controller disconnects for one minute
   (Clicking "Abort" stops early; everything is put back.)
5. At the end:
      "Fix found ... Keep this change permanently?"  -> Yes (can be undone later)
      "No regular Wi-Fi freezes"   -> start your game, leave it in its main menu
                                      (no match), and run the check once more.
      "The PC is not the cause"    -> the log (Log button, top right) lists steps
                                      for the router.


B. Record while playing
-----------------------
1. Open the "Record" tab. Pick your game (or "Auto-detect" for any game) and
   click "Start recording". Leave LagScope open (it can be minimised).
2. Play one or two matches as usual.
3. EVERY time it feels laggy (delayed shots/hit markers, rubber-banding, stutter),
   press F8. You hear a short beep. Pressing a second or two late is fine.
   If the game shows a network warning icon, remember which one.
4. After the matches, click "Stop & build report". The report opens with
   "Open report"; a file named LagScope_....zip is saved next to lagscope.exe.


Automatic recording (optional)
------------------------------
On the Record tab, tick "Record automatically whenever one of these games runs".
LagScope then sits in the notification area (tray, next to the clock) - closing the
window keeps it there - and records every session of a supported game, building
the report when the game exits. Tick "Start LagScope with Windows" to have it
start hidden at logon. Right-click the tray icon to open it, turn auto-record off,
or exit.


Network tests (Network tab)
---------------------------
* Router: shows which router/gateway you have and your ISP, with model-specific tips.
* Latency under load: downloads and uploads as fast as possible for ~12 s each
  while pinging; a grade A+..F tells whether busy connections (downloads, uploads,
  streaming by anyone in the house) will make games lag ("bufferbloat"). Uses up to
  1-2 GB of data on a fast line, so don't run it on a metered connection.
* Wi-Fi neighbourhood: how crowded your Wi-Fi channel is compared with others.
Each test is saved as a session (Sessions tab) with its own report.


Comparing sessions
------------------
On the Sessions tab, tick two sessions and click "Compare" to see them side by
side (e.g. before and after a change) - green = better, red = worse.


C. Send back (if someone is helping you)
----------------------------------------
Send the LagScope_....zip files (the Sessions tab -> "Folder" shows where they
are), plus a few words: what did it feel like, which warning icons did you see?
The Sessions tab lists every recording; "Re-analyze" rebuilds an old report with
the analysis of the LagScope version you are running.


Undo
----
Settings LagScope kept for you are listed at the bottom of the "Wi-Fi check" tab,
with an "Undo all" button.


If something goes wrong
-----------------------
* The window does not open: right-click lagscope.exe > Run as administrator.
* The PC lost power or LagScope was closed during the Wi-Fi check: just start
  lagscope.exe again - any setting that was switched for a test is put back
  automatically.
* F8 does something in the game: pick another mark key on the Record tab.


What LagScope does and does not do
----------------------------------
* While recording it only measures: network packet timing of the game (not the
  contents), pings to your router and the internet, frame times, Wi-Fi, CPU/GPU
  status and how long each driver keeps the CPU busy (driver hiccups). It never
  touches the game itself (no injection, no memory reading), so it is safe with
  anti-cheat systems such as Easy Anti-Cheat.
* LagScope itself uses well under 1% of the CPU; its window redraws at most once a
  second while recording and not at all when idle.
* The latency-under-load test is the only feature that sends real traffic to the
  internet (to Cloudflare's speed-test service), and only when you click it. The
  router lookup only talks to your own router (plus one name lookup).
* Recording also notes disk activity (how much the game reads from disk), to tell
  stutter from asset loading apart from network lag.
* The Wi-Fi check changes settings ONLY after you click Yes, one at a time, and puts
  each one back after its test. The settings it may test: Wi-Fi adapter roaming,
  power saving, throughput acceleration, preferred band, Wi-Fi Direct, Location
  services, Bluetooth, and Windows' Wi-Fi background scanning.
* Privacy: the zip contains timing data, ping results, hardware/driver/settings info
  and process names. Wi-Fi network names, BSSIDs, MAC addresses, the PC name and the
  user name are removed. No packet contents, keystrokes or screenshots.


Command line (optional; lagscope.exe --help lists everything)
  lagscope.exe --console          the text menu instead of the window
  lagscope.exe --record --game cs2 --hotkey F9 --duration 1800
  lagscope.exe --wifi-check       the Wi-Fi check in a console window
  lagscope.exe --undo             undo the settings LagScope kept
  lagscope.exe games              list the game profiles ("auto" = any game)
  lagscope.exe compare <A> <B>    compare two session folders
  lagscope.exe load-test          latency under load (bufferbloat) in the console
  lagscope.exe wifi-scan          Wi-Fi neighbourhood in the console
  lagscope.exe router             router / ISP identification
  lagscope.exe startup on|off     start hidden in the tray with Windows
