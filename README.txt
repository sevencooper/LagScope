LagScope - finds (and fixes) where lag in THE FINALS comes from
================================================================

You need: the PC you play on, connected to your usual Wi-Fi. About 15 minutes for
part A, and a match or two for part B.

Before you start
----------------
* Unzip LagScope.zip anywhere (e.g. the Desktop).
* Double-click lagscope.exe.
    - Windows asks "Do you want to allow this app to make changes?": click Yes
      (it needs administrator rights to measure network and Wi-Fi details).
    - If a blue "Windows protected your PC" box appears: click "More info",
      then "Run anyway" (the program is not code-signed, that is all).
* A black window with a menu opens:
      1  Wi-Fi check & fix
      2  Record while playing
      Q  Quit


A. Wi-Fi check & fix  (do this first - takes about 15 minutes)
---------------------------------------------------------------
1. Close THE FINALS, and pause anything that downloads or streams (Steam
   downloads, OneDrive, videos). Discord can stay open.
2. Start lagscope.exe and press 1.
3. It shows your Wi-Fi adapter and driver, then asks:
      "Check Windows Update for newer Wi-Fi / Bluetooth drivers?"   -> press Y
   If it finds drivers it asks "Install these now?"                  -> press Y
   If it then says "Restart the PC now": restart, start lagscope.exe again,
   press 1 again, and answer N to the driver question this time.
4. It lists the settings it will test and asks
      "Allow these temporary changes?"                               -> press Y
   Each setting is switched for one minute and then put back exactly as it was.
   What you will notice:
      - Wi-Fi drops for a few seconds a few times (when an adapter setting changes)
      - Bluetooth headset / controller disconnects for one minute
   Do not use the PC until it finishes (about 15 minutes). Leave the window open.
   (Pressing Q stops early; everything is put back.)
5. At the end it shows a small table and one of these:
      "Fix found: ..."  It asks "Keep this change permanently?"      -> press Y
                        (can be undone later, see "Undo" below)
      "The PC is not the cause"   -> it lists steps for the router (Wi-Fi box).
      "No regular Wi-Fi freezes"  -> nothing to fix on the Wi-Fi right now.
6. A report opens in your browser and a file named LagScope_....zip appears
   next to lagscope.exe. Keep it - you will send it at the end.
   Press any key to close the window.


B. Record while playing  (after part A)
----------------------------------------
1. Start lagscope.exe again and press 2. Leave the black window open.
2. Start THE FINALS and play one or two matches as usual.
3. EVERY time it feels laggy (delayed shots/hit markers, rubber-banding, stutter),
   press F8. You hear a short beep. Pressing a second or two late is fine.
   If the game shows a red warning icon in the top-right corner, remember which one.
4. After the matches, click on the black LagScope window and press Q.
   It builds a report and another LagScope_....zip file next to lagscope.exe.


C. Send back
------------
Send both LagScope_....zip files (from part A and part B), plus a few words:
did the game feel better after part A? Which red icons did you see, if any?


Undo
----
To undo a setting LagScope kept for you: start lagscope.exe and press 3
("Undo the setting change(s) LagScope made"). This option only appears when
LagScope has changed something.


If something goes wrong
-----------------------
* No menu / window closes immediately: right-click lagscope.exe > Run as administrator.
* The PC lost power or the window was closed during the check: just start
  lagscope.exe again - any setting that was switched for a test is put back
  automatically at start.
* F8 does something in another program: start it from a terminal with
      lagscope.exe --record --hotkey F9


What LagScope does and does not do
----------------------------------
* While recording it only measures: network packet timing of the game (not the
  contents), pings to your router and the internet, frame times, Wi-Fi and CPU/GPU
  status. It never touches the game itself (no injection, no memory reading), so it
  is safe with Easy Anti-Cheat.
* The Wi-Fi check changes settings ONLY after you press Y, one at a time, and puts
  each one back after its test. The settings it may test: Wi-Fi adapter roaming,
  power saving, throughput acceleration, preferred band, Wi-Fi Direct, Location
  services, Bluetooth, and Windows' Wi-Fi background scanning.
* Privacy: the zip contains timing data, ping results, hardware/driver/settings info
  and process names. Wi-Fi network names, BSSIDs, MAC addresses, the PC name and the
  user name are removed. No packet contents, keystrokes or screenshots.


Options (for a terminal; lagscope.exe --help lists all)
  --wifi-check           start the Wi-Fi check directly
  --record               start recording directly
  --hotkey F9            use another mark key
  --duration 1800        stop recording after 30 minutes
  --process Name.exe     watch another game
  --undo                 undo the settings LagScope kept
