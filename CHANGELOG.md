# Changelog

What changed in each version, for people using LagScope. The release workflow copies a
version's section into its GitHub release, so write it for users: what they will notice,
not how it was built. New changes go under "Unreleased"; see [docs/RELEASING.md](docs/RELEASING.md).

## Unreleased

- **Updated the window toolkit** (egui / eframe 0.37).
- **Upload freezes:** when the PC's uploads freeze while the game's data keeps arriving
  (which feels like rubber-banding or being pulled back), the report now says so, counts
  them on the Wi-Fi / network line and explains marked spikes with it. Freezes that start at
  the same point within each second are reported as a 1-second timer (router or adapter
  firmware), not interference.
- **Alt-tab marks:** a lag spike marked while switched away from the game is labelled as such
  and no longer counts towards the headline.
- **Fewer false alarms:** hitches while loading into a match (its first 5 s) are not counted;
  a program is only blamed for stutters when it was busy during them clearly more often than
  otherwise, and "System" (Windows itself) gets advice that makes sense; a laptop GPU at its
  normal temperature target is no longer flagged; long fake "periods" (e.g. every 162 s) are
  no longer reported.

## 0.4.6 (2026-10-22)

- **AI analysis:** every recording now contains `AI_GUIDE.md`, which explains every file
  and column to an AI assistant. "Copy AI prompt" (result screen, Reports > More) copies a
  ready-made prompt to paste into ChatGPT, Claude or similar together with the zip.
- **Copy summary:** the result as plain text, for a chat or forum post.
- **"Bug or idea?"** link at the top right of the window, and in every report.
- **Privacy:** global IPv6 addresses are now removed from recordings (they identify your
  connection like a public IP). See PRIVACY.md, now included in the download.
- **Fixes:** `cpu_cores.csv` and the "disconnected" rows of `wifi.csv` had the wrong number
  of columns for other tools (spreadsheets, Python). "Rebuild report" now also updates the
  recording's zip.

## 0.4.5 (2026-10-22)

- **Desktops on a cable:** lag is blamed on the local network link, not Wi-Fi. Scans from an
  unused Wi-Fi card are ignored. Unstable wired links get advice (powerline, mesh, switch,
  cable, port). The live view shows the cable link.
- **Two routers:** the ISP ping skips a second router (e.g. your own router behind the ISP's
  gateway). The report notes the double NAT.
- **AMD / Intel graphics:** GPU load is shown in the live view, the scorecard and the report.
- **Alt-tab:** frames around switching away from the game no longer count as stutter or
  lower the FPS figures.
- **More accurate checks:** driver-load spikes are judged against the PC's own normal; the
  Memory row names a nearly full commit limit (RAM + page file); GPU heat slowdowns are amber
  only when frequent or during stutters; the graphics card's PCIe link is judged by actual
  bandwidth; RAM channel detection understands AMD and Intel slot names; a desktop on a UPS
  is no longer treated as a laptop.
- **Removed:** "Record automatically", the tray icon and "Start with Windows" (auto-record
  could start a new recording right after you stopped one while the game was closing). A
  logon task left by an older version is deleted on start.
- **Robustness:** an internal error is written to `errors.txt` in the recording instead of
  passing silently. Odd counter values can no longer break the analysis.

## 0.4.3 (2026-10-22)

- **Simpler window:** tabs Record / Wi-Fi fix / Network / Reports, a colour-coded result,
  "Open report" as the main button and the top 3 things to try.
- **Scorecard:** one line per area (Wi-Fi, internet & game server, frames, CPU, GPU, memory,
  disk, drivers, display & power, hardware setup), green or amber, on the result screen, in
  Reports and in the HTML report.
- **New checks:** programs busy during stutters, CPU held back by power or heat, low RAM,
  battery and power plan, monitor below its maximum refresh rate, Ethernet speed, drops and
  errors.
- **Hardware setup:** RAM below its rated speed (XMP/EXPO off), mixed sticks, single
  channel; monitor plugged into the motherboard; graphics card on a slow PCIe link or with a
  lowered power limit; full or spinning system drive; CPU capped by the power plan; old
  graphics driver.
- **Live view** shows disk activity and the busiest background program.
- **Lighter recording:** about 7-8% of one CPU core in total.
- **Advanced:** turn off driver & disk tracing (`--no-kernel-trace`).

## 0.4.0 (2026-10-22, not released separately)

- **Network tab:** router / provider identification, a "busy connection" (bufferbloat) test
  and a scan of nearby Wi-Fi channels.
- **Auto-record** from the tray (removed again in 0.4.5).
- **Compare** two recordings side by side, e.g. before and after a fix.
- **Disk:** asset streaming and slow disk I/O are matched against frame hitches.

## 0.3.0 (2026-10-21, not released separately)

- **Desktop window** instead of the console menu (the console remains with `--console`).
- **Game profiles:** THE FINALS, Counter-Strike 2 and auto-detect; more can be added as JSON
  files.
- **Driver latency:** how long every driver's DPCs and interrupt handlers run
  (LatencyMon-style), and whether long runs line up with stalls.

## 0.2.2 (2026-10-21)

- **Scoring weighs how bad freezes are:** a change that turns ~140 ms freezes into ~30 ms
  blips now counts as a fix.
- **Timing test:** if a freeze keeps its exact beat through a Wi-Fi adapter restart, the
  report says its timer is outside the PC (router side).
- **Fixes:** the power-saving test failed when only selective suspend needed changing; the
  driver and Windows Update result is now saved in the session.

## 0.2.1 (2026-10-21)

- **Lost router pings now count:** a freeze that drops router pings is treated the same as
  one that delays them.
- **Clean baseline:** if the Wi-Fi check sees no freezes at the desktop, it asks for a rerun
  with the game open at the main menu before concluding.

## 0.2.0 (2026-10-21)

- **Wi-Fi check & fix:** about 15 minutes at the desktop. Offers newer Wi-Fi / Bluetooth
  drivers from Windows Update, then switches suspect settings off one at a time for 60 s
  (roaming, power saving, throughput acceleration, preferred band, Wi-Fi Direct, Location,
  Bluetooth, background scanning), reports which one removed the freezes and offers to keep
  it. Interrupted tests are undone at the next start.
- **Driver commands:** the requests Windows sends to the network driver are recorded, and the
  report flags any that keep coming right before freezes.

## 0.1.1 (2026-10-21)

- **Clockwork freezes:** when router ping spikes and game-stream freezes repeat on the same
  exact timer, the report leads with it and lists targeted steps.
- **Fixes:** periodicity detection could report a multiple of the real period; Wi-Fi
  background scans are only blamed when they coincide with spikes.

## 0.1.0 (2026-10-21)

First release: a passive recorder for THE FINALS that records game packet timing, pings,
frames, Wi-Fi and system data while you play, and builds an offline HTML report showing
where each marked lag spike came from.
