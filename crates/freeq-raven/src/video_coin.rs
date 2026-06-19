//! Raven coin video renderer.
//!
//! Activated by `--render-backend coin`. The primary path streams the
//! authored MP4 state loops through ffmpeg and publishes those frames to
//! the live tile. If ffmpeg is unavailable, the backend falls back to the
//! older static PNG renderer so Raven still has a visible avatar.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use bytes::Bytes;
use image::{GenericImageView, RgbaImage, imageops::FilterType};
use iroh_live::media::format::VideoFrame;
use resvg::tiny_skia::{IntSize, Pixmap};
use resvg::usvg;

use crate::video::{
    VIDEO_H, VIDEO_W, VideoTile, composite_overlay, overlay_svg_for_visual_backend,
};

const FPS: u64 = 30;
const FRAME_LEN: usize = VIDEO_W as usize * VIDEO_H as usize * 4;
const CROSSFADE_FRAMES: u32 = 3;

const IDLE_MP4: &[u8] = include_bytes!("../assets/raven-idle-loop.mp4");
const LISTENING_MP4: &[u8] = include_bytes!("../assets/raven-listening-loop.mp4");
const THINKING_MP4: &[u8] = include_bytes!("../assets/raven-thinking-loop.mp4");
const SPEAKING_MP4: &[u8] = include_bytes!("../assets/raven-speaking-loop.mp4");
const VISION_MP4: &[u8] = include_bytes!("../assets/raven-vision-loop.mp4");

const COIN_HEIGHT: u32 = 420;
const COIN_CROP_X_FRAC: f32 = 42.0 / 974.0;
const COIN_CROP_Y_FRAC: f32 = 58.0 / 1024.0;
const COIN_CROP_SIZE_FRAC: f32 = 882.0 / 974.0;
const LIT_BYTES: &[u8] = include_bytes!("../assets/raven-lit.png");
const UNLIT_BYTES: &[u8] = include_bytes!("../assets/raven-unlit.png");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CoinState {
    Idle,
    Listening,
    Thinking,
    Speaking,
    Vision,
}

impl CoinState {
    fn filename(self) -> &'static str {
        match self {
            CoinState::Idle => "raven-idle-loop.mp4",
            CoinState::Listening => "raven-listening-loop.mp4",
            CoinState::Thinking => "raven-thinking-loop.mp4",
            CoinState::Speaking => "raven-speaking-loop.mp4",
            CoinState::Vision => "raven-vision-loop.mp4",
        }
    }

    fn bytes(self) -> &'static [u8] {
        match self {
            CoinState::Idle => IDLE_MP4,
            CoinState::Listening => LISTENING_MP4,
            CoinState::Thinking => THINKING_MP4,
            CoinState::Speaking => SPEAKING_MP4,
            CoinState::Vision => VISION_MP4,
        }
    }
}

#[cfg(test)]
const STATES: [CoinState; 5] = [
    CoinState::Idle,
    CoinState::Listening,
    CoinState::Thinking,
    CoinState::Speaking,
    CoinState::Vision,
];

/// Render Raven as an animated raven coin.
pub(crate) fn render_loop(tile: VideoTile) {
    if let Err(error) = render_mp4_loop(tile.clone()) {
        tracing::warn!(
            error = ?error,
            "coin renderer: MP4 loop renderer unavailable; falling back to static PNG renderer"
        );
        render_png_loop(tile);
    }
}

fn render_mp4_loop(tile: VideoTile) -> Result<()> {
    let assets = StateLoopAssets::create()?;
    let mut reader = FfmpegLoopReader::new(CoinState::Idle, assets.path(CoinState::Idle))?;

    let Some(mut overlay_pixmap) = Pixmap::new(VIDEO_W, VIDEO_H) else {
        anyhow::bail!("allocating overlay pixmap");
    };
    let frame_size = IntSize::from_wh(VIDEO_W, VIDEO_H).context("building frame size")?;
    let mut usvg_opt = usvg::Options::default();
    usvg_opt.fontdb_mut().load_system_fonts();

    let frame_dt = Duration::from_millis(1000 / FPS);
    let started = Instant::now();
    // Ref-counted handle to the last published frame, kept only to seed a
    // crossfade on the rare state change — cloning it is a refcount bump, not
    // a per-frame ~3.7 MB memcpy.
    let mut last_frame: Option<Bytes> = None;
    let mut crossfade: Option<Crossfade> = None;
    tracing::info!("raven coin MP4 renderer started ({VIDEO_W}x{VIDEO_H} @ {FPS}fps)");

    while tile.running.load(Ordering::Relaxed) {
        let tick = Instant::now();
        let t = started.elapsed().as_secs_f32();
        let state = tile_state(&tile);
        if state != reader.state {
            if let Some(from) = last_frame.clone() {
                crossfade = Some(Crossfade::new(from));
            }
            reader.switch(state, assets.path(state))?;
        }

        let mut data = reader.read_frame()?;
        if let Some(fade) = crossfade.as_mut() {
            fade.blend_into(&mut data);
            if fade.is_done() {
                crossfade = None;
            }
        }
        if let Some(overlay_svg) = overlay_svg_for_visual_backend(&tile, t) {
            let Some(mut frame_pixmap) = Pixmap::from_vec(data, frame_size) else {
                anyhow::bail!("ffmpeg frame had unexpected size");
            };
            composite_overlay(
                &mut frame_pixmap,
                &mut overlay_pixmap,
                &overlay_svg,
                &usvg_opt,
            );
            data = frame_pixmap.take();
        }

        let bytes = Bytes::from(data);
        last_frame = Some(bytes.clone());
        let rendered = VideoFrame::new_rgba(bytes, VIDEO_W, VIDEO_H, Duration::ZERO);
        if let Ok(mut g) = tile.latest.lock() {
            *g = Some(rendered);
        }

        if let Some(rest) = frame_dt.checked_sub(tick.elapsed()) {
            std::thread::sleep(rest);
        }
    }

    tracing::info!("raven coin MP4 renderer stopped");
    Ok(())
}

/// Snapshot the tile's audio/vision signals: `(level, peer, thinking,
/// has_vision_thumb)`. Both the MP4 ([`tile_state`]) and PNG
/// ([`render_png_loop`]) renderers read the same four values to drive their
/// state/lighting, so the load-and-clamp idiom lives here once.
fn read_signals(tile: &VideoTile) -> (f32, f32, bool, bool) {
    let level = f32::from_bits(tile.level.load(Ordering::Relaxed)).clamp(0.0, 1.0);
    let peer = f32::from_bits(tile.peer_level.load(Ordering::Relaxed)).clamp(0.0, 1.0);
    let thinking = tile.thinking.load(Ordering::Relaxed);
    let has_vision_thumb = tile
        .vision_thumb
        .lock()
        .ok()
        .map(|g| g.is_some())
        .unwrap_or(false);
    (level, peer, thinking, has_vision_thumb)
}

fn tile_state(tile: &VideoTile) -> CoinState {
    let (level, peer, thinking, has_vision_thumb) = read_signals(tile);
    visual_state(level, peer, thinking, has_vision_thumb)
}

struct Crossfade {
    from: Bytes,
    frame: u32,
}

impl Crossfade {
    fn new(from: Bytes) -> Self {
        Self { from, frame: 0 }
    }

    fn blend_into(&mut self, data: &mut [u8]) {
        self.frame += 1;
        let mix = (self.frame as f32 / CROSSFADE_FRAMES as f32).clamp(0.0, 1.0);
        blend_frames(&self.from, data, mix);
    }

    fn is_done(&self) -> bool {
        self.frame >= CROSSFADE_FRAMES
    }
}

fn blend_frames(from: &[u8], data: &mut [u8], mix: f32) {
    if from.len() != data.len() {
        return;
    }
    let inv = 1.0 - mix.clamp(0.0, 1.0);
    for (src, dst) in from.iter().zip(data.iter_mut()) {
        *dst = (*src as f32 * inv + *dst as f32 * mix).round() as u8;
    }
}

fn visual_state(level: f32, peer: f32, thinking: bool, has_vision_thumb: bool) -> CoinState {
    if has_vision_thumb {
        CoinState::Vision
    } else if level > 0.03 {
        CoinState::Speaking
    } else if thinking {
        CoinState::Thinking
    } else if peer > 0.03 {
        CoinState::Listening
    } else {
        CoinState::Idle
    }
}

struct StateLoopAssets {
    dir: PathBuf,
    idle: PathBuf,
    listening: PathBuf,
    thinking: PathBuf,
    speaking: PathBuf,
    vision: PathBuf,
}

impl StateLoopAssets {
    fn create() -> Result<Self> {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "freeq-raven-state-loops-{}-{now_ms}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

        Ok(Self {
            idle: write_state_asset(&dir, CoinState::Idle)?,
            listening: write_state_asset(&dir, CoinState::Listening)?,
            thinking: write_state_asset(&dir, CoinState::Thinking)?,
            speaking: write_state_asset(&dir, CoinState::Speaking)?,
            vision: write_state_asset(&dir, CoinState::Vision)?,
            dir,
        })
    }

    fn path(&self, state: CoinState) -> &Path {
        match state {
            CoinState::Idle => &self.idle,
            CoinState::Listening => &self.listening,
            CoinState::Thinking => &self.thinking,
            CoinState::Speaking => &self.speaking,
            CoinState::Vision => &self.vision,
        }
    }
}

impl Drop for StateLoopAssets {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn write_state_asset(dir: &Path, state: CoinState) -> Result<PathBuf> {
    let path = dir.join(state.filename());
    fs::write(&path, state.bytes()).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

struct FfmpegLoopReader {
    state: CoinState,
    /// Path of the loop ffmpeg is currently decoding — always the file for
    /// `state`. Owned here so `read_frame` can restart on stream-end without
    /// the caller re-threading a path that must match `state`.
    path: PathBuf,
    child: Child,
    stdout: ChildStdout,
}

impl FfmpegLoopReader {
    fn new(state: CoinState, path: &Path) -> Result<Self> {
        let (child, stdout) = spawn_ffmpeg_loop(state, path)?;
        Ok(Self {
            state,
            path: path.to_path_buf(),
            child,
            stdout,
        })
    }

    fn switch(&mut self, state: CoinState, path: &Path) -> Result<()> {
        if self.state == state {
            return Ok(());
        }
        tracing::debug!(?state, "coin renderer: switching MP4 state");
        self.restart(state, path)
    }

    fn read_frame(&mut self) -> Result<Vec<u8>> {
        let mut frame = vec![0; FRAME_LEN];
        if let Err(error) = self.stdout.read_exact(&mut frame) {
            tracing::warn!(
                state = ?self.state,
                error = ?error,
                "coin renderer: MP4 stream ended; restarting loop"
            );
            let (state, path) = (self.state, self.path.clone());
            self.restart(state, &path)?;
            self.stdout.read_exact(&mut frame).with_context(|| {
                format!("reading first frame after restarting {:?}", self.state)
            })?;
        }
        Ok(frame)
    }

    fn restart(&mut self, state: CoinState, path: &Path) -> Result<()> {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let (child, stdout) = spawn_ffmpeg_loop(state, path)?;
        self.state = state;
        self.path = path.to_path_buf();
        self.child = child;
        self.stdout = stdout;
        Ok(())
    }
}

impl Drop for FfmpegLoopReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_ffmpeg_loop(state: CoinState, path: &Path) -> Result<(Child, ChildStdout)> {
    let vf = format!(
        "fps={FPS},scale={VIDEO_W}:{VIDEO_H}:force_original_aspect_ratio=decrease,pad={VIDEO_W}:{VIDEO_H}:(ow-iw)/2:(oh-ih)/2,format=rgba"
    );
    let mut child = Command::new("ffmpeg")
        .arg("-v")
        .arg("error")
        .arg("-stream_loop")
        .arg("-1")
        .arg("-i")
        .arg(path)
        .arg("-an")
        .arg("-vf")
        .arg(vf)
        .arg("-pix_fmt")
        .arg("rgba")
        .arg("-f")
        .arg("rawvideo")
        .arg("pipe:1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("starting ffmpeg for {state:?} loop"))?;
    let stdout = child
        .stdout
        .take()
        .with_context(|| format!("capturing ffmpeg stdout for {state:?} loop"))?;
    Ok((child, stdout))
}

struct CoinImage {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

fn render_png_loop(tile: VideoTile) {
    let Some(unlit) = prepare_coin(UNLIT_BYTES) else {
        tracing::error!("coin renderer: failed to decode unlit raven coin");
        return;
    };
    let Some(lit) = prepare_coin(LIT_BYTES) else {
        tracing::error!("coin renderer: failed to decode lit raven coin");
        return;
    };
    tracing::info!("raven coin PNG fallback renderer started");

    let Some(mut frame) = Pixmap::new(VIDEO_W, VIDEO_H) else {
        tracing::error!("coin renderer: failed to allocate {VIDEO_W}x{VIDEO_H} pixmap");
        return;
    };
    let Some(mut overlay_pixmap) = Pixmap::new(VIDEO_W, VIDEO_H) else {
        tracing::error!("coin renderer: failed to allocate overlay pixmap");
        return;
    };
    let mut usvg_opt = usvg::Options::default();
    usvg_opt.fontdb_mut().load_system_fonts();

    let frame_dt = Duration::from_millis(1000 / FPS);
    let started = Instant::now();
    while tile.running.load(Ordering::Relaxed) {
        let tick = Instant::now();
        let t = started.elapsed().as_secs_f32();

        let (level, peer, thinking, has_vision_thumb) = read_signals(&tile);
        let lighting = coin_lighting(
            visual_state(level, peer, thinking, has_vision_thumb),
            level,
            peer,
        );
        draw_background(frame.data_mut(), lighting.energy);
        draw_coin_glow(frame.data_mut(), lighting.glow_mix, lighting.energy);
        draw_coin(frame.data_mut(), &unlit, &lit, lighting.lit_mix);

        if let Some(overlay_svg) = overlay_svg_for_visual_backend(&tile, t) {
            composite_overlay(&mut frame, &mut overlay_pixmap, &overlay_svg, &usvg_opt);
        }

        let data = Bytes::copy_from_slice(frame.data());
        let rendered = VideoFrame::new_rgba(data, VIDEO_W, VIDEO_H, Duration::ZERO);
        if let Ok(mut g) = tile.latest.lock() {
            *g = Some(rendered);
        }

        if let Some(rest) = frame_dt.checked_sub(tick.elapsed()) {
            std::thread::sleep(rest);
        }
    }

    tracing::info!("raven coin PNG fallback renderer stopped");
}

struct CoinLighting {
    lit_mix: f32,
    glow_mix: f32,
    energy: f32,
}

/// Map the already-classified [`CoinState`] to PNG-renderer lighting. The
/// state/priority ladder lives solely in [`visual_state`]; this just lights
/// each state, scaling speaking/listening by their continuous level so the two
/// renderers can't drift on which state wins.
fn coin_lighting(state: CoinState, level: f32, peer: f32) -> CoinLighting {
    let (lit_mix, energy) = match state {
        CoinState::Vision => (0.9, 0.8),
        CoinState::Speaking => (
            (0.42 + level * 0.72).min(1.0),
            (0.45 + level * 0.55).min(1.0),
        ),
        CoinState::Thinking => (0.68, 0.62),
        CoinState::Listening => (
            (0.24 + peer * 0.58).min(0.88),
            (0.30 + peer * 0.5).min(0.72),
        ),
        CoinState::Idle => (0.24, 0.22),
    };

    CoinLighting {
        lit_mix: lit_mix.clamp(0.0, 1.0),
        glow_mix: (lit_mix * 0.82 + energy * 0.18).clamp(0.0, 1.0),
        energy: energy.clamp(0.0, 1.0),
    }
}

fn prepare_coin(bytes: &[u8]) -> Option<CoinImage> {
    let img = image::load_from_memory(bytes).ok()?;
    let (src_w, src_h) = img.dimensions();
    let crop_x = ((src_w as f32) * COIN_CROP_X_FRAC).round() as u32;
    let crop_y = ((src_h as f32) * COIN_CROP_Y_FRAC).round() as u32;
    let crop_size = ((src_w as f32) * COIN_CROP_SIZE_FRAC).round() as u32;
    let crop_x = crop_x.min(src_w.saturating_sub(1));
    let crop_y = crop_y.min(src_h.saturating_sub(1));
    let crop_size = crop_size
        .min(src_w.saturating_sub(crop_x))
        .min(src_h.saturating_sub(crop_y));
    let img = img.crop_imm(crop_x, crop_y, crop_size, crop_size);
    let (src_w, src_h) = img.dimensions();
    let width = (COIN_HEIGHT as f32 * src_w as f32 / src_h as f32).round() as u32;
    let mut resized: RgbaImage = img
        .resize_exact(width, COIN_HEIGHT, FilterType::Lanczos3)
        .to_rgba8();
    apply_coin_mask(&mut resized);
    Some(CoinImage {
        width,
        height: COIN_HEIGHT,
        rgba: resized.into_raw(),
    })
}

fn apply_coin_mask(img: &mut RgbaImage) {
    let w = img.width() as f32;
    let h = img.height() as f32;
    let cx = w * 0.5;
    let cy = h * 0.505;
    let radius = w.min(h) * 0.485;
    let soft = 18.0;

    for y in 0..img.height() {
        for x in 0..img.width() {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let d = (dx * dx + dy * dy).sqrt();
            let edge = ((radius + soft - d) / soft).clamp(0.0, 1.0);
            let edge = edge * edge * (3.0 - 2.0 * edge);
            let px = img.get_pixel_mut(x, y);
            px.0[3] = ((px.0[3] as f32) * edge).round() as u8;
        }
    }
}

fn draw_background(data: &mut [u8], energy: f32) {
    let w = VIDEO_W as usize;
    let h = VIDEO_H as usize;
    let teal = 0.55;
    let ember = 0.45;
    for y in 0..h {
        let ny = y as f32 / VIDEO_H as f32;
        for x in 0..w {
            let nx = x as f32 / VIDEO_W as f32;
            let dx = nx - 0.5;
            let dy = ny - 0.48;
            let radial = (1.0 - (dx * dx * 2.6 + dy * dy * 4.4).sqrt()).clamp(0.0, 1.0);
            let vignette =
                (1.0 - ((dx * dx * 1.8 + dy * dy * 2.7).sqrt() - 0.32) * 1.1).clamp(0.22, 1.0);
            let idx = (y * w + x) * 4;
            data[idx] = ((4.0 + radial * 24.0 + ember * energy * 10.0) * vignette) as u8;
            data[idx + 1] = ((7.0 + radial * 22.0 + teal * energy * 14.0) * vignette) as u8;
            data[idx + 2] = ((13.0 + radial * 31.0 + teal * energy * 20.0) * vignette) as u8;
            data[idx + 3] = 255;
        }
    }
}

fn draw_coin_glow(data: &mut [u8], glow_mix: f32, energy: f32) {
    let w = VIDEO_W as i32;
    let h = VIDEO_H as i32;
    let cx = VIDEO_W as f32 * 0.5;
    let cy = VIDEO_H as f32 * 0.5;
    let radius = 255.0 + energy * 40.0;
    let strength = 0.10 + glow_mix * 0.34;
    for y in 0..h {
        for x in 0..w {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let d = (dx * dx + dy * dy).sqrt();
            if d > radius {
                continue;
            }
            let falloff = 1.0 - d / radius;
            let add = falloff * falloff * strength;
            let idx = ((y as u32 * VIDEO_W + x as u32) * 4) as usize;
            add_rgb(data, idx, 18.0 * add, 120.0 * add, 135.0 * add);
            add_rgb(
                data,
                idx,
                150.0 * add * glow_mix,
                76.0 * add * glow_mix,
                22.0 * add * glow_mix,
            );
        }
    }
}

fn draw_coin(data: &mut [u8], unlit: &CoinImage, lit: &CoinImage, lit_mix: f32) {
    let dst_w = unlit.width as i32;
    let dst_h = unlit.height as i32;
    let x0 = (VIDEO_W as i32 - dst_w) / 2;
    let y0 = (VIDEO_H as i32 - dst_h) / 2;
    let inv_mix = 1.0 - lit_mix;

    for dy in 0..dst_h {
        let y = y0 + dy;
        if !(0..VIDEO_H as i32).contains(&y) {
            continue;
        }
        let sy = dy as u32;
        for dx in 0..dst_w {
            let x = x0 + dx;
            if !(0..VIDEO_W as i32).contains(&x) {
                continue;
            }
            let sx = dx as u32;
            let src_idx = ((sy * unlit.width + sx) * 4) as usize;
            let alpha = unlit.rgba[src_idx + 3] as f32 / 255.0;
            if alpha <= 0.001 {
                continue;
            }
            let dst_idx = ((y as u32 * VIDEO_W + x as u32) * 4) as usize;
            for c in 0..3 {
                let src = unlit.rgba[src_idx + c] as f32 * inv_mix
                    + lit.rgba[src_idx + c] as f32 * lit_mix;
                data[dst_idx + c] =
                    (data[dst_idx + c] as f32 * (1.0 - alpha) + src * alpha).round() as u8;
            }
        }
    }
}

fn add_rgb(data: &mut [u8], idx: usize, r: f32, g: f32, b: f32) {
    data[idx] = (data[idx] as f32 + r).min(255.0) as u8;
    data[idx + 1] = (data[idx + 1] as f32 + g).min(255.0) as u8;
    data[idx + 2] = (data[idx + 2] as f32 + b).min(255.0) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coin_assets_decode_to_matching_buffers() {
        let unlit = prepare_coin(UNLIT_BYTES).expect("unlit coin decodes");
        let lit = prepare_coin(LIT_BYTES).expect("lit coin decodes");
        assert_eq!(unlit.width, lit.width);
        assert_eq!(unlit.height, lit.height);
        assert_eq!(unlit.rgba.len(), lit.rgba.len());
        assert!(unlit.rgba.chunks_exact(4).any(|p| p[3] > 0));
    }

    #[test]
    fn coin_frame_draws_non_black_pixels() {
        let unlit = prepare_coin(UNLIT_BYTES).expect("unlit coin decodes");
        let lit = prepare_coin(LIT_BYTES).expect("lit coin decodes");
        let mut data = vec![0; (VIDEO_W * VIDEO_H * 4) as usize];
        draw_background(&mut data, 0.5);
        draw_coin_glow(&mut data, 0.8, 0.6);
        draw_coin(&mut data, &unlit, &lit, 0.7);
        assert!(
            data.chunks_exact(4)
                .any(|p| p[0] > 80 || p[1] > 80 || p[2] > 80)
        );
    }

    #[test]
    fn mp4_state_assets_are_embedded() {
        for state in STATES {
            let bytes = state.bytes();
            assert!(bytes.len() > 1024, "{state:?} loop should not be empty");
            assert_eq!(&bytes[4..8], b"ftyp", "{state:?} should be an MP4");
        }
    }

    #[test]
    fn visual_state_priority_is_stable() {
        assert_eq!(visual_state(0.0, 0.0, false, false), CoinState::Idle);
        assert_eq!(visual_state(0.0, 0.2, false, false), CoinState::Listening);
        assert_eq!(visual_state(0.0, 0.2, true, false), CoinState::Thinking);
        assert_eq!(visual_state(0.5, 0.2, true, false), CoinState::Speaking);
        assert_eq!(visual_state(0.5, 0.2, true, true), CoinState::Vision);
    }

    #[test]
    fn blend_frames_interpolates_pixels() {
        let from = [0, 10, 20, 255];
        let mut data = [100, 110, 120, 255];
        blend_frames(&from, &mut data, 0.5);
        assert_eq!(data, [50, 60, 70, 255]);
    }
}
