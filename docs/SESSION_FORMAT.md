# LagScope session format

A LagScope recording is one folder (`LagScope_YYYYMMDD_HHMMSS`, also shipped as a zip) of
plain CSV and JSON files. This page describes every file and column. It is also copied
into each recording as `AI_GUIDE.md`, so an AI assistant given the zip knows how to read it.

## Ground rules

- **Time.** Every CSV starts with `t_us`: microseconds since the recording started, all on
  one high-resolution clock (QPC), so rows from different files can be compared directly.
  Wall-clock start: `meta.json` → `start_local` (local time) and `start_unix_ms`.
- **Empty cell** = not available at that moment (counter not readable, not sampled in that
  tick). It is never zero.
- **Rates.** Most system data is sampled at 4 Hz (every 250 ms), process lists and disk / driver
  summaries at 1 Hz. Pings run at 20 Hz per target. Game packets and frames are per event.
- **Passive.** LagScope never touches the game process (anti-cheat safe): it sees the game's
  packets and frames from Windows' event tracing (ETW), not from inside the game.
- **Privacy.** Wi-Fi names, MAC addresses, BSSIDs (hashed per session), the PC name, the
  user name and global IPv6 addresses are removed. Public IPs of the game server and of the
  ISP's routers on the path remain (needed for the diagnosis).

## Where to start

Read these small files first; they hold LagScope's own conclusions:

| File | What it is |
|---|---|
| `meta.json` | Session facts: tool version, game profile, connection (`route`: adapter, `is_wifi`, gateway, link speed), hardware snapshot (`hardware`, `displays`, `nvidia`, `ram_mb`, `logical_cpus`), power state, router identification, collector problems, LagScope's own CPU use. |
| `analysis.json` | LagScope's analysis: `headline`, `health` (scorecard: area, status ok/warn/na, text), `findings`, `recommendations`, `verdicts` (one per lag spike the user marked: culprit, category, confidence, evidence; category `away` = marked while switched away from the game), `anomalies` (every detected event, see below), correlations, `periodicity` (repeating events per series: period, share on the beat, events, `typical_ms`; network series allow skipped beats), `second_timer` (router freezes starting at the same point within a second: [that many, of all]), `frames` and `stream` statistics, `probes` (per ping target, with `jitter_ms`), `timer` (`first_hop` / `beyond` / null), `pauses` (every game-data pause of 100 ms+ during play: `t`, `dir` down/up, `ms`, `what` held/lost/not sent, `cause` = verdict category), `good_for` (gaming / video calls / streaming: `grade` bad..great or null, `grade_text`, `note`) with `good_for_from` (the busy-connection test used), `phases` (match play; the first 5 s of each are not judged for frames) and `away` (times the game was not in front) with `away_switches`. |
| `events.csv` | Timeline in words: collectors starting, game start/exit, server detected, route, marks, Wi-Fi events, power and link changes. |
| `marks.csv` | When the user pressed the lag-spike key: `t_us,source,n`. People press 0.5-3 s after they notice; look at the seconds **before** each mark. |
| `report.html` | The same analysis as an interactive page (charts); open it in a browser. |

### analysis.json → anomalies

`{t, dur, kind, target, value, detail, in_match}`; `t` and `dur` in µs. `in_match` is false
outside match play (menus, loading) and while the player was switched away from the game; such
events are shown but not counted. Kinds:

| kind | meaning, `value` |
|---|---|
| InGap / OutGap | no game packets from / to the server for longer than normal, ms |
| FrameHitch | one frame took much longer than normal, ms |
| Stall | the whole PC woke up late (timer test), ms |
| ProbeSpike / ProbeLoss / RttPlateau | ping spike (ms), lost pings (count), ping stuck high (ms); `target` = gateway, hop2, internet, server |
| WifiScan / WifiRoam / WifiDisconnect / WifiQualityDrop / WifiRetries | Wi-Fi events (ignored on wired connections) |
| TrafficBurst | other programs moving a lot of data, Mbit/s; `target` = top program |
| DpcSpike | driver (DPC/interrupt) time a full CPU core above normal, % of all CPUs |
| DriverLatency | one driver routine ran ≥ 1 ms (graphics drivers ≥ 2 ms), ms; `target` = driver |
| DiskSlow / GamePaging | one disk I/O ≥ 100 ms; the game read ≥ 8 MB from disk via page faults in one second |
| GpuLimit | NVIDIA GPU clocks limited by heat / power brake, °C |
| Paging | heavy paging while RAM was low |
| GameBurst | the game pinged all its regions at once (hundreds of tiny packets) |
| UploadStall | the path through the router froze (router and farther pings stalled together) while the game's incoming stream kept arriving: the PC's own packets were held back, ms |

## Network

| File | Columns |
|---|---|
| `game_packets.csv` | Every packet of the game: `dir` S = sent / R = received, `proto` U = UDP / T = TCP, `size` bytes, `remote_ip`, `remote_port`, `local_port`. The real-time server stream is the remote UDP endpoint sending the most packets (see `analysis.json` → `stream.endpoints`). Small request/response pairs to many endpoints are the game's region pings. |
| `probes.csv` | Pings: `target` (gateway = your router; hop2 = first public ISP router; internet = 1.1.1.1; server = game server or, if it ignores pings, the last router before it that answers TTL-limited pings), `ip`, `status` (0 = reply; 11013 = TTL expired, counts as a reply in TTL mode; 99999 = not sent, all slots busy), `rtt_us` (-1 = lost), `ttl`. |
| `traceroute.csv` | Path to 1.1.1.1 and to each game server: `dst,ttl,hop_ip,rtt_ms` (empty hop = no answer). |
| `proc_net.csv` | Per second and process: bytes sent/received over UDP and TCP, packet counts. Shows downloads / uploads competing with the game. |
| `link.csv` | The network adapter: first row at start, then a row on every change or new error: `up`, `rx_mbps`/`tx_mbps` (link speed; Wi-Fi rates change constantly and only the first row is meaningful), error / discard counts since the previous row. |

## Wi-Fi (empty or absent on wired connections)

| File | Columns |
|---|---|
| `wifi.csv` | 4 Hz: `connected`, `bssid_hash` (changes = roamed to another access point or band), `phy` (Windows DOT11_PHY_TYPE number), `channel`, `rssi_dbm`, `quality` (0-100), `rx_mbps`/`tx_mbps` (link rate), then frame counter deltas since the previous row (`d_tx`, `d_rx`, `d_retry`, ...). Some drivers report a constant RSSI or impossible retry counts; `analysis.json` says so when detected. |
| `wifi_events.csv` | Windows WLAN notifications: `source` (acm / msm), `code`, `name` (scan_complete, roaming_start, disconnected, signal_quality_change with the new quality in `data`, ...). |
| `wlan_etw.csv` | Windows Wi-Fi service trace events (`provider`, `id`, `event`, `values`): background scans, network list updates, roaming decisions. |
| `wifi_driver.csv` | Requests to the network driver (NDIS OIDs): `id`, `pid` of the requesting process, `event`, `oid` (hex), `values`. Regular bursts before freezes can mean a program polls the Wi-Fi adapter. |

## Frames and focus

| File | Columns |
|---|---|
| `frames.csv` | Each frame the game presents (DirectX): `t_us` = Present call, `present_us` = time spent inside Present (long = waiting for the GPU / vsync), `sync_interval`, `flags`, `swapchain`, `result`. Frame time = gap between consecutive `t_us`. Games rendering through Vulkan / OpenGL without DXGI have no rows. |
| `focus.csv` | Foreground changes: `game` 1 = the game is the active window, 0 = something else (`process`). Frames around switching away are not judged. |

## CPU, memory, GPU

| File | Columns |
|---|---|
| `cpu.csv` | 4 Hz. `total_pct` / `utility_pct` (% of all CPUs), `max_core_pct` (busiest logical CPU), `dpc_pct` / `interrupt_pct` (driver time, % of all CPUs; × `logical_cpus` / 100 = cores), `perf_pct` (speed vs. base clock, > 100 = boost), `freq_mhz`, `dpc_rate`, `interrupt_rate`, `ctx_switch_rate`, `run_queue`, `avail_mb` (free RAM), `pages_in_rate`, `disk_idle_pct`, `disk_ms_per_io`; the game's `game_cpu_pct` (% of all CPUs), `game_private_mb`, `game_page_fault_rate`, `game_io_bps`, `game_threads`, `game_priority`; `perf_limit_pct` (Windows' "% Performance Limit": 100 = not limited, lower = held back by power / heat), `commit_pct` (% of RAM + page file promised to programs). |
| `cpu_cores.csv` | 4 Hz: `core` ("group,number", quoted), `pct` busy. Recordings before 0.4.6 left the name unquoted, so their rows have four fields: `t_us`, group, number, `pct`. |
| `proc_cpu.csv` | 1 Hz: up to 5 other programs using ≥ 10% of **one** core: `name`, `cpu_pct` (% of one core). |
| `gpu.csv` | NVIDIA only (4 Hz while the game runs, else every 5 s): `util_pct`, `mem_util_pct`, `gfx_mhz`, `mem_mhz`, `power_w`, `power_limit_w`, `temp_c`, `pstate`, `reasons` (clock limits: idle, sw_power_cap = normal power target, hw_slowdown / hw_thermal / sw_thermal / hw_power_brake = real throttling), `pcie_tx_kbs`, `pcie_rx_kbs`, `vram_used_mb`, `enc_pct` (video encoder, e.g. recording), `dec_pct`, `pcie_gen`, `pcie_width` (link at that moment; drops when idle). |
| `gpu_engines.csv` | 1 Hz, any GPU vendor (Windows counters): `scope` (`adapter_<id>` = whole GPU, `game` = the game's share), `engine` (3D, Copy, VideoEncode, ...), `util_pct`. |

## Drivers, disk and system stalls (absent when "Driver & disk tracing" was off)

| File | Columns |
|---|---|
| `drivers_1s.csv` | Per second and driver: number, total and longest run of DPCs and interrupt handlers (µs). LatencyMon-style; long runs delay everything on that CPU core (network, audio, frames). |
| `driver_long.csv` | Every single driver run ≥ 0.5 ms: `kind` D = DPC / I = interrupt, `driver`, `dur_us`, `cpu`. |
| `disk_1s.csv` | Per second: reads / writes and MB, slowest I/O (ms), the game's hard page faults and MB read through them (asset streaming or memory pressure). |
| `disk_slow.csv` | Each disk I/O ≥ 50 ms: `kind` R/W, `size_kb`, `ms`, `disk` number. |
| `stalls.csv` / `stall_1s.csv` | A top-priority thread asks to wake every 1 ms; `late_us` = how late it woke (> 4 ms logged). Per second: wake-ups, max and p99 lateness, counts over 2 ms and 8 ms. Late wake-ups = the whole PC was busy in drivers / firmware. |
| `etw_health*.csv` | Trace sessions' lost events (non-zero = data has gaps). |

## sysinfo/ (snapshot at the start)

`system.txt` (Windows version, CPU, board / BIOS, RAM sticks, GPUs and drivers, network
adapters with all advanced driver settings, power management, TCP settings, gaming-related
registry values, notable services and installed network software), `processes_at_start.txt`,
`ipconfig_all.txt`, `netsh_*.txt` (Wi-Fi interface, driver capabilities, settings),
`powercfg_*.txt` (power plan, Wi-Fi and processor power settings), `eventlog_*.txt`
(Windows event logs around the session: WLAN, network, system, application).

## Known limits

- The game's own state (tick rate, its netcode buffers, what it showed on screen) is not
  visible; a stall with clean pings and a steady stream may still be server-side.
- Upload-direction delays cannot be measured from the PC, only download-side gaps.
- GPU temperature, clock limits and power come from NVIDIA's interface; AMD / Intel GPUs only
  have `gpu_engines.csv`.
- Pings to routers can be deprioritised by the routers themselves; one slow hop with clean
  hops after it is usually not a real problem.
