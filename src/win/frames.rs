//! Frame pacing from the Microsoft-Windows-DXGI provider (Present_Start = 42,
//! Present_Stop = 43). The interval between successive Present calls of the game is the
//! CPU-side frame time; a long interval is a local hitch. The Present duration shows time
//! spent blocked inside Present (vsync / GPU back-pressure).

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;

use super::Running;
use super::etw::{self, Event, ProviderSpec, RealtimeSession, SchemaCache};
use crate::app::App;
use crate::row;
use crate::session::Sink;

struct Pending {
    t: i64,
    sync: u64,
    flags: u64,
    swapchain: u64,
}

struct FrameState {
    app: Arc<App>,
    cache: SchemaCache,
    sink: Arc<Sink>,
    pending: HashMap<u32, Pending>,
    last_start: Option<i64>,
    sec: i64,
    count: u32,
    max_interval: i64,
}

impl FrameState {
    fn on_event(&mut self, ev: &Event) {
        if !self.app.is_game_pid(ev.pid()) {
            // Auto-detect: count who presents frames, the busiest one is the game.
            if ev.id() == 42 && self.app.cfg.processes.is_empty() && self.app.game_pid.load(std::sync::atomic::Ordering::Relaxed) == 0 {
                *self.app.auto_presents.lock().unwrap().entry(ev.pid()).or_default() += 1;
            }
            return;
        }
        let t = self.app.session.clock.us_from_raw(ev.timestamp());
        match ev.id() {
            42 => {
                let Some(schema) = self.cache.get(ev) else { return };
                let d = ev.data();
                self.pending.insert(
                    ev.tid(),
                    Pending {
                        t,
                        sync: schema.uint(d, "SyncInterval").unwrap_or(0),
                        flags: schema.uint(d, "Flags").unwrap_or(0),
                        swapchain: schema.uint(d, "pIDXGISwapChain").unwrap_or(0),
                    },
                );
                self.live(t);
            }
            43 => {
                let Some(p) = self.pending.remove(&ev.tid()) else { return };
                let result = self.cache.get(ev).and_then(|s| s.uint(ev.data(), "Result")).unwrap_or(0);
                row!(
                    self.sink,
                    "{},{},{},{},{:x},{:x}",
                    p.t,
                    t - p.t,
                    p.sync,
                    p.flags,
                    p.swapchain & 0xffff_ffff,
                    result
                );
            }
            _ => {}
        }
    }

    fn live(&mut self, t: i64) {
        if let Some(prev) = self.last_start {
            self.max_interval = self.max_interval.max(t - prev);
        }
        self.last_start = Some(t);
        let sec = t.div_euclid(1_000_000);
        if sec != self.sec {
            let mut live = self.app.live.lock().unwrap();
            live.fps = self.count as f64;
            live.max_frame_ms = self.max_interval as f64 / 1000.0;
            live.frames_updated_us = self.app.session.clock.now_us();
            drop(live);
            self.sec = sec;
            self.count = 0;
            self.max_interval = 0;
        }
        self.count += 1;
    }
}

pub fn start(app: Arc<App>) -> Result<Running> {
    let sink = app.session.sink("frames", "t_us,present_us,sync_interval,flags,swapchain,result");
    let health = app.session.sink("etw_health_frames", "t_us,session,events_lost,rt_buffers_lost,buffers_written");
    let mut state = FrameState {
        app: app.clone(),
        cache: SchemaCache::default(),
        sink,
        pending: HashMap::new(),
        last_start: None,
        sec: 0,
        count: 0,
        max_interval: 0,
    };
    let spec = ProviderSpec {
        name: "DXGI",
        guid: etw::DXGI,
        level: 5,
        any_keyword: u64::MAX,
        event_ids: vec![42, 43],
    };
    let sess = RealtimeSession::start("LagScope-Frames", &[spec], move |ev| state.on_event(ev))?;
    Ok(super::etw_running("frames", app, sess, health))
}
