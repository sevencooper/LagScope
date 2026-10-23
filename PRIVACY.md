# Privacy

LagScope runs only on your PC. It has no account, no telemetry and no update check, and it
never uploads anything. A recording leaves your PC only if you send its zip to someone (a
friend, a forum, an AI assistant).

## What a recording contains

Each recording is a folder (and a zip of it) next to `lagscope.exe`, or in Documents if
that folder is not writable. [docs/SESSION_FORMAT.md](docs/SESSION_FORMAT.md) describes every
file. In short:

- **Network timing.** When the game sent and received packets (sizes, not contents), with
  the game servers' IP addresses and ports; how much data each program on the PC sent and
  received per second (program names, no addresses or contents); pings and traceroutes to
  your router, your provider's first routers, 1.1.1.1 and the game server.
- **Your provider's routers.** The public IP addresses of the routers right after yours,
  and the reverse-DNS name of the first one (e.g. `ae-1.chcgil01.example-isp.net`, where `chcgil` stands for Chicago).
  These names often reveal your provider and roughly which city or region you are in.
- **Wi-Fi.** Signal, channel, link rate, scans and roaming. The network name is removed and
  the access point's address (BSSID) is replaced with a per-recording code.
- **The PC.** Hardware (CPU, GPU, RAM sticks, drives, monitors), driver versions and
  settings of network adapters, power settings, some game-related Windows settings, CPU /
  GPU / memory / disk load, how long drivers ran, the names of running programs.
- **Windows event logs** from 2 minutes before the recording until its end (System,
  Application, Wi-Fi, network profile), e.g. driver warnings and application errors.
- **Your marks**: when you pressed the lag-spike key. No other keys are recorded.

## What is removed or never recorded

- Removed from everything LagScope saves: Wi-Fi network names, MAC addresses, the PC name,
  your user name and domain, and global IPv6 addresses (from version 0.4.6; earlier
  recordings may contain them in `sysinfo/ipconfig_all.txt`).
- Not recorded at all: packet contents, screen contents, keystrokes (other than the mark
  key), files, browsing, anything inside the game process.
- Your public IPv4 address is not recorded, since the PC only sees its router's private
  addresses. (If your PC connects to the internet with no router at all, its public
  address would appear in `sysinfo/ipconfig_all.txt`.)

## What LagScope sends over the network

- **While recording:** pings (ICMP) to your router, your provider's first router, 1.1.1.1
  and the game server; traceroutes to 1.1.1.1 and to the game server; one reverse-DNS
  lookup of your provider's first router; a lookup of your router's model on the local
  network (its UPnP description and web page). Nothing else.
- **Only when you click it:** the "busy connection" test downloads and uploads data
  from / to Cloudflare's speed test (up to 2 GB), and the Wi-Fi fix can ask Windows
  Update for newer drivers.

## What LagScope changes on your PC

Nothing while recording. The Wi-Fi fix changes Wi-Fi settings only after you say yes,
puts each one back after its test, and journals every change in `%ProgramData%\LagScope`
so an interrupted test is undone at the next start. `%ProgramData%\LagScope\settings.json`
remembers the last game you picked and whether driver & disk tracing is on.

## Sharing with an AI assistant

"Copy AI prompt" only copies text; you decide whether to paste it and attach the zip. If
you do, the recording goes to that AI provider and their privacy policy applies. The
provider router addresses and names above appear in several files (pings, traceroutes,
events, meta.json) and are needed to tell your provider's problems apart from the game
server's, so if they are a concern, share recordings only with people and services you trust
rather than posting them publicly.
