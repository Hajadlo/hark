//! The overlay owns its connection and never binds a seat or requests focus.
use anyhow::{Context, Result, ensure};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{EventLoop, channel},
        calloop_wayland_source::WaylandSource,
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{
        Shm, ShmHandler,
        slot::{Buffer, SlotPool},
    },
};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_output, wl_region, wl_shm, wl_surface},
};
use wayland_protocols_plasma::output_order::v1::client::kde_output_order_v1::{
    self, KdeOutputOrderV1,
};

const WIDTH: u32 = 224;
const HEIGHT: u32 = 68;
const FADE_MS: u64 = 150;
const MESSAGE_MS: u64 = 1500;

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Idle,
    Recording { mode: String, toggle: bool },
    Processing,
    Done,
    NothingHeard,
    Failed,
}
impl State {
    fn label(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Recording { .. } => "recording",
            Self::Processing => "processing",
            Self::Done => "done",
            Self::NothingHeard => "nothing-heard",
            Self::Failed => "failed",
        }
    }
}
struct Model {
    phase: State,
    visual: State,
    since: u64,
}
impl Model {
    fn new() -> Self {
        Self {
            phase: State::Idle,
            visual: State::Idle,
            since: 0,
        }
    }
    fn set(&mut self, state: State, now: u64) {
        if self.phase == state && !matches!(state, State::NothingHeard | State::Failed) {
            return;
        }
        if state != State::Done {
            self.visual = state.clone();
        }
        self.phase = state;
        self.since = now;
    }
    fn alpha(&self, now: u64) -> f32 {
        let elapsed = now.saturating_sub(self.since);
        let fade = match self.phase {
            State::Idle => return 0.0,
            State::Done => elapsed,
            State::NothingHeard | State::Failed => elapsed.saturating_sub(MESSAGE_MS),
            _ => return 1.0,
        };
        (1.0 - fade as f32 / FADE_MS as f32).clamp(0.0, 1.0)
    }
    fn tick(&mut self, now: u64) {
        if self.phase != State::Idle && self.alpha(now) == 0.0 {
            self.set(State::Idle, now);
        }
    }
}

#[derive(Default)]
pub struct Level(AtomicU32);
impl Level {
    pub fn publish(&self, rms: f32) {
        self.0.store(rms.to_bits(), Ordering::Relaxed);
    }
    fn read(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
}
#[derive(Default)]
pub struct Smoother(f32);
impl Smoother {
    pub fn update(&mut self, rms: f32, seconds: f32) -> f32 {
        let target = if rms.is_finite() {
            rms.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let seconds = if seconds.is_finite() {
            seconds.max(0.0)
        } else {
            0.0
        };
        let tau = if target > self.0 { 0.045 } else { 0.18 };
        self.0 += (target - self.0) * (1.0 - (-seconds / tau).exp());
        self.0
    }
}
fn pulse(rms: f32) -> f32 {
    if !rms.is_finite() || rms <= 0.002 {
        return 0.0;
    }
    ((rms / 0.002).ln() / 5.2).clamp(0.0, 1.0)
}
fn choose_output<'a>(
    available: &'a [String],
    order: &[String],
    requested: Option<&str>,
) -> Option<&'a str> {
    requested
        .and_then(|name| available.iter().find(|a| a.as_str() == name))
        .or_else(|| {
            order
                .iter()
                .find_map(|name| available.iter().find(|a| *a == name))
        })
        .map(String::as_str)
}

const RESTART_BACKOFF: Duration = Duration::from_secs(5);
struct Worker {
    sender: channel::Sender<State>,
    thread: std::thread::JoinHandle<()>,
}
struct Supervisor {
    worker: Option<Worker>,
    retry_at: Instant,
}
impl Supervisor {
    fn new(now: Instant) -> Self {
        Self {
            worker: None,
            retry_at: now,
        }
    }
    fn set(&mut self, state: State, now: Instant, spawn: impl FnOnce() -> std::io::Result<Worker>) {
        if let Some(worker) = &self.worker {
            if !worker.thread.is_finished() && worker.sender.send(state.clone()).is_ok() {
                return;
            }
            self.worker = None;
        }
        if now < self.retry_at {
            return;
        }
        // Bound creation attempts even if the new connection fails immediately.
        self.retry_at = now + RESTART_BACKOFF;
        if let Ok(worker) = spawn() {
            let _ = worker.sender.send(state);
            self.worker = Some(worker);
        }
    }
}
pub struct Overlay {
    supervisor: Option<Mutex<Supervisor>>,
    output: Option<String>,
    reported: Arc<AtomicBool>,
    pub level: Arc<Level>,
}
impl Overlay {
    pub fn new(enabled: bool, output: Option<String>) -> Self {
        Self {
            supervisor: enabled.then(|| Mutex::new(Supervisor::new(Instant::now()))),
            output,
            reported: Arc::new(AtomicBool::new(false)),
            level: Arc::new(Level::default()),
        }
    }
    fn spawn(&self) -> std::io::Result<Worker> {
        let (sender, receiver) = channel::channel();
        let level = self.level.clone();
        let output = self.output.clone();
        let reported = self.reported.clone();
        let thread = std::thread::Builder::new()
            .name("hark-overlay".into())
            .spawn(move || {
                let result =
                    std::panic::catch_unwind(|| worker(receiver, level, output, &reported));
                let error = match result {
                    Ok(Ok(())) => return,
                    Ok(Err(e)) => format!("{e:#}"),
                    Err(_) => "rendering thread failed".into(),
                };
                if !reported.swap(true, Ordering::Relaxed) {
                    eprintln!("overlay unavailable: {error}; dictation continues");
                }
            });
        match thread {
            Ok(thread) => Ok(Worker { sender, thread }),
            Err(e) => {
                if !self.reported.swap(true, Ordering::Relaxed) {
                    eprintln!("overlay unavailable: {e}; dictation continues");
                }
                Err(e)
            }
        }
    }
    pub fn set(&self, state: State) {
        // No joins, compositor calls, sleeps or lock waits on the dictation loop.
        if let Some(supervisor) = &self.supervisor
            && let Ok(mut supervisor) = supervisor.try_lock()
        {
            supervisor.set(state, Instant::now(), || self.spawn());
        }
    }
}

fn system_font() -> Option<fontdue::Font> {
    let output = std::process::Command::new("fc-match")
        .args(["-f", "%{file}", "sans"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?;
    let bytes = std::fs::read(path.trim()).ok()?;
    fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).ok()
}
fn paint(rgba: [u8; 4], opacity: f32) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(rgba[0], rgba[1], rgba[2], (rgba[3] as f32 * opacity) as u8);
    paint
}
fn pill(
    pixmap: &mut Pixmap,
    bounds: (f32, f32, f32, f32),
    rgba: [u8; 4],
    opacity: f32,
    scale: f32,
) {
    let (x, y, w, h) = bounds;
    let mut path = PathBuilder::new();
    let r = h.min(w) / 2.0;
    path.move_to(x + r, y);
    path.line_to(x + w - r, y);
    path.cubic_to(
        x + w - r + r * 0.5523,
        y,
        x + w,
        y + r - r * 0.5523,
        x + w,
        y + r,
    );
    path.line_to(x + w, y + h - r);
    path.cubic_to(
        x + w,
        y + h - r + r * 0.5523,
        x + w - r + r * 0.5523,
        y + h,
        x + w - r,
        y + h,
    );
    path.line_to(x + r, y + h);
    path.cubic_to(
        x + r - r * 0.5523,
        y + h,
        x,
        y + h - r + r * 0.5523,
        x,
        y + h - r,
    );
    path.line_to(x, y + r);
    path.cubic_to(x, y + r - r * 0.5523, x + r - r * 0.5523, y, x + r, y);
    path.close();
    if let Some(path) = path.finish() {
        pixmap.fill_path(
            &path,
            &paint(rgba, opacity),
            FillRule::Winding,
            Transform::from_scale(scale, scale),
            None,
        );
    }
}
fn text(
    pixmap: &mut Pixmap,
    font: Option<&fontdue::Font>,
    value: &str,
    placement: (f32, f32, f32),
    rgba: [u8; 4],
    opacity: f32,
    scale: f32,
) {
    let (x, baseline, size) = placement;
    let Some(font) = font else {
        return;
    };
    let mut x = x * scale;
    for character in value.chars() {
        let (metrics, bitmap) = font.rasterize(character, size * scale);
        let left = x.round() as i32 + metrics.xmin;
        let top = (baseline * scale).round() as i32 - metrics.height as i32 - metrics.ymin;
        for (i, coverage) in bitmap.into_iter().enumerate() {
            let px = left + (i % metrics.width) as i32;
            let py = top + (i / metrics.width) as i32;
            if px < 0 || py < 0 || px >= pixmap.width() as i32 || py >= pixmap.height() as i32 {
                continue;
            }
            let a = ((coverage as f32 / 255.0) * rgba[3] as f32 * opacity) as u32;
            let index = py as usize * pixmap.width() as usize + px as usize;
            let dst = pixmap.pixels_mut()[index];
            let blend =
                |src: u8, dst: u8| ((src as u32 * a + dst as u32 * (255 - a) + 127) / 255) as u8;
            pixmap.pixels_mut()[index] = tiny_skia::PremultipliedColorU8::from_rgba(
                blend(rgba[0], dst.red()),
                blend(rgba[1], dst.green()),
                blend(rgba[2], dst.blue()),
                (a + (dst.alpha() as u32 * (255 - a) + 127) / 255) as u8,
            )
            .unwrap();
        }
        x += metrics.advance_width;
    }
}
fn draw(
    state: &State,
    rms: f32,
    now: u64,
    alpha: f32,
    scale: f32,
    font: Option<&fontdue::Font>,
) -> Result<Pixmap> {
    let mut pixmap = Pixmap::new(
        (WIDTH as f32 * scale).ceil() as u32,
        (HEIGHT as f32 * scale).ceil() as u32,
    )
    .context("overlay image allocation")?;
    if matches!(state, State::Idle) || alpha == 0.0 {
        return Ok(pixmap);
    }
    for spread in (1..=10).rev() {
        let s = spread as f32;
        pill(
            &mut pixmap,
            (12.0 - s, 14.0 - s / 2.0, 200.0 + s * 2.0, 44.0 + s),
            [0, 0, 0, 5],
            alpha,
            scale,
        );
    }
    pill(
        &mut pixmap,
        (12.0, 12.0, 200.0, 44.0),
        [165, 173, 190, 100],
        alpha,
        scale,
    );
    pill(
        &mut pixmap,
        (12.8, 12.8, 198.4, 42.4),
        [23, 26, 33, 245],
        alpha,
        scale,
    );
    let transform = Transform::from_scale(scale, scale);
    let p = pulse(rms);
    let (label, colour) = match state {
        State::Recording { mode, .. } => (mode.as_str(), [243, 245, 250, 255]),
        State::NothingHeard => ("Nothing heard", [204, 210, 221, 255]),
        State::Failed => ("Failed", [255, 154, 161, 255]),
        _ => ("Processing…", [232, 235, 243, 255]),
    };
    if matches!(state, State::Processing | State::Done) {
        let angle = now as f32 / 900.0 * std::f32::consts::TAU;
        let mut path = PathBuilder::new();
        for n in 0..=24 {
            let a = angle + n as f32 / 24.0 * std::f32::consts::TAU * 0.72;
            let (x, y) = (37.0 + a.cos() * 7.0, 34.0 + a.sin() * 7.0);
            if n == 0 {
                path.move_to(x, y);
            } else {
                path.line_to(x, y);
            }
        }
        if let Some(path) = path.finish() {
            pixmap.stroke_path(
                &path,
                &paint([179, 191, 215, 255], alpha),
                &Stroke {
                    width: 2.0,
                    line_cap: tiny_skia::LineCap::Round,
                    ..Stroke::default()
                },
                transform,
                None,
            );
        }
    } else {
        let recording = matches!(state, State::Recording { .. });
        let dot = if recording || matches!(state, State::Failed) {
            [248, 91, 110, 255]
        } else {
            [170, 180, 197, 255]
        };
        if recording && let Some(path) = PathBuilder::from_circle(37.0, 34.0, 8.0 + p * 5.0) {
            pixmap.fill_path(
                &path,
                &paint([248, 91, 110, 35 + (p * 30.0) as u8], alpha),
                FillRule::Winding,
                transform,
                None,
            );
        }
        if let Some(path) =
            PathBuilder::from_circle(37.0, 34.0, 4.0 + if recording { p * 2.5 } else { 0.0 })
        {
            pixmap.fill_path(
                &path,
                &paint(dot, alpha),
                FillRule::Winding,
                transform,
                None,
            );
        }
        if recording {
            for (i, weight) in [0.45, 0.8, 1.0, 0.65].into_iter().enumerate() {
                let h = 3.0 + p * 15.0 * weight;
                pill(
                    &mut pixmap,
                    (177.0 + i as f32 * 5.0, 34.0 - h / 2.0, 2.0, h),
                    [248, 115, 131, 200],
                    alpha,
                    scale,
                );
            }
        }
    }
    text(
        &mut pixmap,
        font,
        label,
        (59.0, 39.0, 13.5),
        colour,
        alpha,
        scale,
    );
    if matches!(state, State::Recording { toggle: true, .. }) {
        text(
            &mut pixmap,
            font,
            "tap to stop",
            (101.0, 38.0, 9.5),
            [180, 188, 204, 255],
            alpha,
            scale,
        );
    }
    Ok(pixmap)
}

struct Ui {
    registry: RegistryState,
    outputs: OutputState,
    shm: Shm,
    compositor: CompositorState,
    shell: LayerShell,
    pool: SlotPool,
    buffers: Vec<Buffer>,
    layer: Option<LayerSurface>,
    configured: bool,
    mapped: bool,
    target: Option<wl_output::WlOutput>,
    scale: i32,
    order: Vec<String>,
    pending_order: Vec<String>,
    requested: Option<String>,
    model: Model,
    level: Arc<Level>,
    font: Option<fontdue::Font>,
    clock: Instant,
    exit: bool,
    next_frame: u64,
    last_drawn: State,
}
impl Ui {
    fn now(&self) -> u64 {
        self.clock.elapsed().as_millis() as u64
    }
    fn unmap(&mut self) {
        if self.layer.take().is_some() {
            eprintln!("overlay unmapped");
            self.buffers.clear();
            self.configured = false;
            self.mapped = false;
        }
    }
    fn update(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        self.model.tick(self.now());
        if self.model.phase == State::Idle {
            self.unmap();
            return Ok(());
        }
        let outputs: Vec<_> = self.outputs.outputs().collect();
        let names: Vec<_> = outputs
            .iter()
            .filter_map(|o| self.outputs.info(o).and_then(|info| info.name))
            .collect();
        let name = choose_output(&names, &self.order, self.requested.as_deref());
        let target = name
            .and_then(|name| {
                outputs
                    .iter()
                    .find(|o| self.outputs.info(o).and_then(|i| i.name).as_deref() == Some(name))
            })
            .cloned();
        if self.layer.is_some() && self.target != target {
            self.unmap();
        }
        if self.layer.is_none() {
            self.target = target;
            self.scale = self
                .target
                .as_ref()
                .and_then(|o| self.outputs.info(o))
                .map_or(1, |i| i.scale_factor.max(1));
            let surface = self.compositor.create_surface(qh);
            // An empty region is different from None (which means all pixels).
            let region = self.compositor.wl_compositor().create_region(qh, ());
            surface.set_input_region(Some(&region));
            region.destroy();
            let layer = self.shell.create_layer_surface(
                qh,
                surface,
                Layer::Overlay,
                Some("hark-recording"),
                self.target.as_ref(),
            );
            layer.set_anchor(Anchor::BOTTOM);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            layer.set_exclusive_zone(0);
            // 12 px shadow padding puts the pill itself 32 logical px above the panel.
            layer.set_margin(0, 0, 20, 0);
            layer.set_size(WIDTH, HEIGHT);
            layer.commit();
            eprintln!(
                "overlay surface created on {} (ceiling scale {}), overlay layer, click-through, keyboard=none",
                name.unwrap_or("compositor-choice"),
                self.scale
            );
            self.layer = Some(layer);
        }
        if !self.configured {
            return Ok(());
        }
        if self.now() < self.next_frame {
            return Ok(());
        }
        let scale = self.scale.max(1);
        let pixmap = draw(
            &self.model.visual,
            self.level.read(),
            self.now(),
            self.model.alpha(self.now()),
            scale as f32,
            self.font.as_ref(),
        )?;
        let width = pixmap.width() as i32;
        let height = pixmap.height() as i32;
        let mut free = self
            .buffers
            .iter()
            .position(|buffer| self.pool.canvas(buffer).is_some());
        if free.is_none() && self.buffers.len() < 3 {
            let (buffer, _) =
                self.pool
                    .create_buffer(width, height, width * 4, wl_shm::Format::Argb8888)?;
            self.buffers.push(buffer);
            free = Some(self.buffers.len() - 1);
        }
        let Some(index) = free else {
            self.next_frame = self.now() + 33;
            return Ok(());
        };
        if let Some(canvas) = self.pool.canvas(&self.buffers[index]) {
            for (dst, src) in canvas.chunks_exact_mut(4).zip(pixmap.pixels()) {
                let argb = (src.alpha() as u32) << 24
                    | (src.red() as u32) << 16
                    | (src.green() as u32) << 8
                    | src.blue() as u32;
                dst.copy_from_slice(&argb.to_ne_bytes());
            }
        }
        let surface = self.layer.as_ref().unwrap().wl_surface();
        surface.set_buffer_scale(scale);
        surface.damage_buffer(0, 0, width, height);
        self.buffers[index].attach_to(surface)?;
        surface.commit();
        self.next_frame = self.now() + 33;
        if !self.mapped {
            self.mapped = true;
            eprintln!("overlay mapped ({})", self.model.phase.label());
        }
        if self.last_drawn != self.model.phase {
            eprintln!("overlay visible: {}", self.model.phase.label());
            self.last_drawn = self.model.phase.clone();
        }
        Ok(())
    }
}
fn worker(
    receiver: channel::Channel<State>,
    level: Arc<Level>,
    requested: Option<String>,
    reported: &AtomicBool,
) -> Result<()> {
    let conn = Connection::connect_to_env().context("Wayland connection")?;
    let (globals, mut queue) = registry_queue_init::<Ui>(&conn)?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh)?;
    let shell = LayerShell::bind(&globals, &qh).context("wlr-layer-shell")?;
    let shm = Shm::bind(&globals, &qh)?;
    let pool = SlotPool::new((WIDTH * HEIGHT * 4) as usize, &shm)?;
    let _order: Option<KdeOutputOrderV1> = globals.bind(&qh, 1..=1, ()).ok();
    let mut ui = Ui {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        shm,
        compositor,
        shell,
        pool,
        buffers: vec![],
        layer: None,
        configured: false,
        mapped: false,
        target: None,
        scale: 1,
        order: vec![],
        pending_order: vec![],
        requested,
        model: Model::new(),
        level,
        font: system_font(),
        clock: Instant::now(),
        exit: false,
        next_frame: 0,
        last_drawn: State::Idle,
    };
    queue.roundtrip(&mut ui)?;
    let mut event_loop: EventLoop<Ui> = EventLoop::try_new()?;
    WaylandSource::new(conn.clone(), queue).insert(event_loop.handle())?;
    event_loop
        .handle()
        .insert_source(receiver, |event, _, ui| match event {
            channel::Event::Msg(state) => {
                if ui.model.phase != state {
                    eprintln!("overlay state: {}", state.label());
                }
                ui.model.set(state, ui.now());
                ui.next_frame = 0;
            }
            channel::Event::Closed => ui.exit = true,
        })
        .map_err(|e| anyhow::anyhow!("overlay channel: {e}"))?;
    reported.store(false, Ordering::Relaxed); // A recovered connection starts a new failure period.
    while !ui.exit {
        ui.update(&qh)?;
        conn.flush()?;
        // No timer, frame requests or polling when idle. Output events and the
        // daemon channel wake us; visible animation alone gets a 30 Hz deadline.
        let timeout = if ui.model.phase == State::Idle {
            None
        } else if !ui.configured {
            Some(Duration::from_millis(33))
        } else {
            Some(Duration::from_millis(
                ui.next_frame.saturating_sub(ui.now()),
            ))
        };
        event_loop.dispatch(timeout, &mut ui)?;
    }
    ui.unmap();
    conn.flush()?;
    Ok(())
}
impl CompositorHandler for Ui {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        factor: i32,
    ) {
        if self
            .layer
            .as_ref()
            .is_some_and(|l| l.wl_surface() == surface)
            && self.scale != factor.max(1)
        {
            self.scale = factor.max(1);
            self.buffers.clear();
        }
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}
impl OutputHandler for Ui {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}
impl LayerShellHandler for Ui {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        if self.layer.as_ref() == Some(layer) {
            // Output removal can close a layer before OutputState sees the
            // registry removal. Re-select after the queued events are drained.
            self.unmap();
        }
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        _: LayerSurfaceConfigure,
        _: u32,
    ) {
        if self.layer.as_ref() == Some(layer) {
            self.configured = true;
        }
    }
}
impl ShmHandler for Ui {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}
impl ProvidesRegistryState for Ui {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState];
}
impl Dispatch<KdeOutputOrderV1, ()> for Ui {
    fn event(
        state: &mut Self,
        _: &KdeOutputOrderV1,
        event: kde_output_order_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            kde_output_order_v1::Event::Output { output_name } => {
                state.pending_order.push(output_name)
            }
            kde_output_order_v1::Event::Done => {
                state.order = std::mem::take(&mut state.pending_order)
            }
            _ => {}
        }
    }
}
delegate_compositor!(Ui);
delegate_output!(Ui);
delegate_layer!(Ui);
delegate_shm!(Ui);
delegate_registry!(Ui);
wayland_client::delegate_noop!(Ui: ignore wl_region::WlRegion);

pub fn previews(directory: &Path) -> Result<()> {
    std::fs::create_dir_all(directory)?;
    let font = system_font();
    let states = [
        (
            "recording-low",
            State::Recording {
                mode: "clean".into(),
                toggle: false,
            },
            0.003,
        ),
        (
            "recording-medium",
            State::Recording {
                mode: "clean".into(),
                toggle: false,
            },
            0.03,
        ),
        (
            "recording-high",
            State::Recording {
                mode: "raw".into(),
                toggle: false,
            },
            0.2,
        ),
        (
            "toggle",
            State::Recording {
                mode: "raw".into(),
                toggle: true,
            },
            0.03,
        ),
        ("processing", State::Processing, 0.0),
        ("nothing-heard", State::NothingHeard, 0.0),
        ("failed", State::Failed, 0.0),
    ];
    for (scale, scale_name) in [(1.0, "1.0"), (1.25, "1.25")] {
        for (background, dark) in [("light", false), ("dark", true)] {
            for (name, state, rms) in &states {
                let chip = draw(state, *rms, 400, 1.0, scale, font.as_ref())?;
                let mut image = Pixmap::new((280.0 * scale) as u32, (110.0 * scale) as u32)
                    .context("preview allocation")?;
                for y in 0..image.height() {
                    for x in 0..image.width() {
                        let check = ((x as f32 / scale / 12.0) as u32
                            + (y as f32 / scale / 12.0) as u32)
                            .is_multiple_of(2);
                        let v = if dark {
                            if check { 35 } else { 44 }
                        } else if check {
                            236
                        } else {
                            249
                        };
                        let index = (y * image.width() + x) as usize;
                        image.pixels_mut()[index] =
                            tiny_skia::PremultipliedColorU8::from_rgba(v, v, v, 255).unwrap();
                    }
                }
                image.draw_pixmap(
                    (28.0 * scale) as i32,
                    (21.0 * scale) as i32,
                    chip.as_ref(),
                    &tiny_skia::PixmapPaint::default(),
                    Transform::identity(),
                    None,
                );
                image.save_png(directory.join(format!("{name}-{scale_name}-{background}.png")))?;
            }
        }
    }
    ensure!(
        font.is_some(),
        "previews rendered without text: fontconfig/font unavailable"
    );
    println!("overlay previews: 28 PNGs, scales 1.0/1.25, light/dark backgrounds");
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fake_worker(log: std::sync::mpsc::Sender<State>) -> std::io::Result<Worker> {
        let (sender, receiver) = channel::channel();
        let thread = std::thread::spawn(move || {
            if let Ok(state) = receiver.recv() {
                let _ = log.send(state);
            }
        });
        Ok(Worker { sender, thread })
    }
    #[test]
    fn dead_worker_restarts_lazily_with_latest_state_after_backoff() {
        let now = Instant::now();
        let mut supervisor = Supervisor::new(now);
        let (tx, rx) = std::sync::mpsc::channel();
        supervisor.set(State::Failed, now, || fake_worker(tx.clone()));
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            State::Failed
        );
        // The fake consumes one state, then dies, like a dropped Wayland connection.
        let wait_end = Instant::now() + Duration::from_secs(1);
        while !supervisor.worker.as_ref().unwrap().thread.is_finished() {
            assert!(Instant::now() < wait_end);
            std::thread::sleep(Duration::from_millis(1));
        }
        supervisor.set(
            State::Done,
            now + RESTART_BACKOFF - Duration::from_millis(1),
            || panic!("overlay spun during backoff"),
        );
        assert!(supervisor.worker.is_none());
        supervisor.set(State::Processing, now + RESTART_BACKOFF, || fake_worker(tx));
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            State::Processing
        );
    }
    #[test]
    fn spawn_failures_are_rate_limited_without_a_retry_timer() {
        let now = Instant::now();
        let mut supervisor = Supervisor::new(now);
        let attempts = std::cell::Cell::new(0);
        for elapsed in [0, 1, 100, 4999, 5000, 5001] {
            supervisor.set(State::Failed, now + Duration::from_millis(elapsed), || {
                attempts.set(attempts.get() + 1);
                Err(std::io::Error::other("synthetic worker failure"))
            });
        }
        assert_eq!(attempts.get(), 2);
        assert!(supervisor.worker.is_none());
    }
    #[test]
    fn smoothing_attack_release_and_callback_duration() {
        let mut smoother = Smoother::default();
        let attack = smoother.update(0.1, 0.045);
        assert!((attack - 0.1 * (1.0 - (-1.0f32).exp())).abs() < 0.00001);
        let release = smoother.update(0.0, 0.18);
        assert!((release - attack * (-1.0f32).exp()).abs() < 0.00001);
        let mut many = Smoother::default();
        for _ in 0..9 {
            many.update(0.1, 0.005);
        }
        assert!((many.0 - attack).abs() < 0.00001);
        assert_eq!(many.update(f32::NAN, 0.0), many.0);
        assert!(many.update(10.0, 1.0) <= 1.0);
    }
    #[test]
    fn pulse_mapping_is_bounded_and_monotonic() {
        assert_eq!(pulse(0.0), 0.0);
        assert_eq!(pulse(f32::NAN), 0.0);
        let values = [0.003, 0.03, 0.2].map(pulse);
        assert!(values[0] > 0.0 && values[0] < values[1] && values[1] < values[2]);
        assert_eq!(pulse(100.0), 1.0);
    }
    #[test]
    fn output_order_override_fallback_and_hotplug() {
        let available = vec!["HDMI-A-1".into(), "DP-4".into(), "DP-2".into()];
        let order = vec!["DP-4".into(), "DP-2".into(), "HDMI-A-1".into()];
        assert_eq!(choose_output(&available, &order, None), Some("DP-4"));
        assert_eq!(
            choose_output(&available, &order, Some("DP-2")),
            Some("DP-2")
        );
        assert_eq!(
            choose_output(&available, &order, Some("missing")),
            Some("DP-4")
        );
        assert_eq!(choose_output(&available[2..], &order, None), Some("DP-2"));
        assert_eq!(choose_output(&available, &[], None), None);
        assert_eq!(choose_output(&[], &order, Some("DP-4")), None);
    }
    #[test]
    fn processing_done_fade_and_message_expiry() {
        let mut model = Model::new();
        assert_eq!(model.alpha(0), 0.0);
        model.set(
            State::Recording {
                mode: "clean".into(),
                toggle: false,
            },
            100,
        );
        assert_eq!(model.alpha(300), 1.0);
        model.set(State::Processing, 400);
        model.set(State::Done, 1000);
        assert_eq!(model.visual, State::Processing);
        assert_eq!(model.alpha(1075), 0.5);
        model.tick(1149);
        assert_eq!(model.phase, State::Done);
        model.tick(1150);
        assert_eq!(model.phase, State::Idle);
        for state in [State::NothingHeard, State::Failed] {
            model.set(state.clone(), 2000);
            model.tick(3500);
            assert_eq!(model.phase, state);
            assert_eq!(model.alpha(3500), 1.0);
            assert_eq!(model.alpha(3575), 0.5);
            model.tick(3650);
            assert_eq!(model.phase, State::Idle);
        }
    }
    #[test]
    fn new_recording_interrupts_message_or_fade() {
        let mut model = Model::new();
        model.set(State::Failed, 0);
        let recording = State::Recording {
            mode: "raw".into(),
            toggle: true,
        };
        model.set(recording.clone(), 100);
        model.tick(2000);
        assert_eq!(model.phase, recording);
        assert_eq!(model.alpha(2000), 1.0);
        model.set(State::Processing, 2100);
        model.set(State::Done, 2200);
        model.set(
            State::Recording {
                mode: "clean".into(),
                toggle: false,
            },
            2250,
        );
        assert_eq!(model.alpha(2400), 1.0);
    }
    #[test]
    fn repeated_error_restarts_message_duration() {
        let mut model = Model::new();
        model.set(State::Failed, 0);
        model.set(State::Failed, 1400);
        model.tick(1650);
        assert_eq!(model.phase, State::Failed);
        assert_eq!(model.alpha(2900), 1.0);
        model.tick(3050);
        assert_eq!(model.phase, State::Idle);
    }
    #[test]
    fn renderer_supports_fractional_scale_and_missing_font() {
        for scale in [1.0, 1.25, 2.0] {
            let image = draw(&State::Processing, 0.0, 500, 1.0, scale, None).unwrap();
            assert_eq!(image.width(), (WIDTH as f32 * scale) as u32);
            assert!(image.pixels().iter().any(|p| p.alpha() > 0));
            let idle = draw(&State::Idle, 0.0, 500, 1.0, scale, None).unwrap();
            assert!(idle.pixels().iter().all(|p| p.alpha() == 0));
        }
    }
}
