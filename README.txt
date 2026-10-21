LagScope - lag spike recorder for THE FINALS
=============================================

What it does
------------
While you play, LagScope records (once per packet / frame / few milliseconds):
  * every network packet between THE FINALS and its server (timing only, not contents)
  * continuous pings to your router, your ISP, the internet and the game server path
  * frame times, Wi-Fi link quality and Wi-Fi scan activity, CPU / GPU / driver stalls
  * which other programs use the network at the same time
When you stop it, it builds a report that says WHERE each lag spike came from:
your Wi-Fi, your ISP, the route to the game server, the server, or your PC.

It is read-only: it changes no settings and never touches the game process
(no injection, no memory reading), so it is safe with Easy Anti-Cheat.

How to use
----------
1. Unzip anywhere (e.g. Desktop) and double-click lagscope.exe.
   - Windows will ask for administrator permission: click Yes (needed to trace
     network packets and frames).
   - If SmartScreen says "Windows protected your PC": click "More info" ->
     "Run anyway" (the program is not code-signed).
2. Leave the black window open and start THE FINALS (order does not matter).
3. Play normally. EVERY time you feel a hiccup / desync / rubber-band, press F8.
   You'll hear a short beep. Pressing a second or two late is fine.
   If you notice a red icon in the top-right of the game (packet loss, jitter,
   latency, server, ...), write down which one - it helps.
4. After a few matches, click the LagScope window and press Q.
   It builds the report (opens in your browser) and a zip file next to the exe:
       LagScope_YYYYMMDD_HHMMSS.zip   <- send this file back.

Good test plan
--------------
  A. 2-3 matches as you normally play (Wi-Fi), marking every hiccup with F8.
  B. Same, but with an Ethernet cable or USB phone tethering, if at all possible.
  C. A game that feels fine, for comparison:
       lagscope.exe --process OtherGame.exe

Options (run from a terminal: lagscope.exe --help)
  --hotkey F9            use another key if F8 is bound in-game
  --duration 1800        stop automatically after 30 minutes
  --process Name.exe     watch another game

Privacy
-------
The zip contains timing data, ping results, hardware/driver info and process names.
Wi-Fi network names, BSSIDs, MAC addresses and host names are removed or hashed.
No packet contents, keystrokes or screenshots are recorded.

Optional: richer Wi-Fi data
---------------------------
Windows 11 hides Wi-Fi signal strength / rates from programs unless Location
services are on. LagScope still records link quality and scans without it, but if
you are OK with it, before a test session turn on:
  Settings > Privacy & security > Location > Location services: On
  and "Let desktop apps access your location": On
(You can turn it off again afterwards.)
