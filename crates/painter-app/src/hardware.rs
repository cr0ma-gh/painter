//! The hardware profile: one place deciding how much Painter trades quality
//! for memory and GPU load, from the machine (RAM, GPU type and limits,
//! `--low-gpu`) or the Performance Profile picked in settings. It sets the GPU
//! canvas's render targets and atlas, the undo history's RAM budget and when
//! background tabs are suspended.
//!
//! The older per-part environment variables still win over it:
//! `PAINTER_LOW_VRAM`, `PAINTER_ATLAS`, `PAINTER_LOW_RAM`,
//! `PAINTER_HISTORY_RAM_MB`, `PAINTER_TAB_SUSPEND_MIN` and `PAINTER_TAB_RAM_MB`.

use std::sync::atomic::{AtomicU8, Ordering};

use eframe::wgpu;
use painter_render::RenderProfile;
use ui_workspace::PerformanceProfile;

use crate::app::PainterApp;
use crate::i18n::tr;

const GIB: u64 = 1 << 30;
/// At most this much RAM (ThinkPad X200 class: 2–4 GB) is a legacy machine.
const LEGACY_RAM: u64 = 4 * GIB;
/// GPUs whose 2D textures can't reach this are downlevel (OpenGL 3.3 / DirectX 10 class).
const DOWNLEVEL_TEXTURE: u32 = 4096;

/// The profile in effect, the choice resolved against the hardware.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tier {
    /// Full-canvas targets, float strokes, standard atlas and RAM budgets.
    HighQuality,
    /// Viewport-sized render targets, somewhat smaller RAM budgets.
    Balanced,
    /// Viewport targets, compact atlas, 8-bit strokes where float blending is
    /// slow, and the low-RAM budgets.
    Legacy,
}

impl Tier {
    /// The settings entry this tier corresponds to.
    pub(crate) fn profile(self) -> PerformanceProfile {
        match self {
            Self::HighQuality => PerformanceProfile::HighQuality,
            Self::Balanced => PerformanceProfile::Balanced,
            Self::Legacy => PerformanceProfile::Legacy,
        }
    }
}

/// The tier in effect for the whole app (see [`set_tier`]).
static TIER: AtomicU8 = AtomicU8::new(0);

/// The tier in effect: [`Tier::HighQuality`] until [`set_tier`].
pub(crate) fn tier() -> Tier {
    match TIER.load(Ordering::Relaxed) {
        1 => Tier::Balanced,
        2 => Tier::Legacy,
        _ => Tier::HighQuality,
    }
}

/// Makes `tier` the one new histories and the tab suspension policy follow.
pub(crate) fn set_tier(tier: Tier) {
    TIER.store(tier as u8, Ordering::Relaxed);
}

/// What the GPU in use offers, as far as the profile cares.
#[derive(Clone, Debug)]
pub(crate) struct GpuInfo {
    pub(crate) name: String,
    pub(crate) device_type: wgpu::DeviceType,
    pub(crate) backend: wgpu::Backend,
    /// The device's largest 2D texture edge.
    pub(crate) max_texture_2d: u32,
    /// `Rgba16Float` can be rendered to and blended (the stroke targets).
    pub(crate) float_blend: bool,
}

impl GpuInfo {
    pub(crate) fn new(adapter: &wgpu::Adapter, device: &wgpu::Device) -> Self {
        let info = adapter.get_info();
        let f16 = adapter.get_texture_format_features(wgpu::TextureFormat::Rgba16Float);
        Self {
            name: info.name,
            device_type: info.device_type,
            backend: info.backend,
            max_texture_2d: device.limits().max_texture_dimension_2d,
            float_blend: f16.flags.contains(wgpu::TextureFormatFeatureFlags::BLENDABLE)
                && f16.allowed_usages.contains(wgpu::TextureUsages::RENDER_ATTACHMENT),
        }
    }

    /// Desktop OpenGL: wgpu found no DirectX 12 / Vulkan / Metal (Intel GMA
    /// 4500, HD 3000), or compatibility mode is on.
    fn gl_fallback(&self) -> bool {
        self.backend == wgpu::Backend::Gl && !cfg!(any(target_os = "android", target_os = "ios"))
    }
}

/// What's known of the machine.
#[derive(Clone, Debug, Default)]
pub(crate) struct Hardware {
    /// Physical memory, when the system says.
    pub(crate) ram_bytes: Option<u64>,
    /// The GPU in use (`None`: no wgpu canvas).
    pub(crate) gpu: Option<GpuInfo>,
    /// `--low-gpu` / `--gpu-compat` or `PAINTER_LOW_GPU`.
    pub(crate) low_gpu_requested: bool,
}

impl Hardware {
    /// This machine, using `gpu`.
    pub(crate) fn detect(gpu: Option<GpuInfo>) -> Self {
        let args: Vec<String> = std::env::args().collect();
        let low_gpu_requested = crate::low_gpu_requested(&args, std::env::var_os("PAINTER_LOW_GPU").as_deref());
        Self { ram_bytes: total_ram_bytes(), gpu, low_gpu_requested }
    }

    /// The tier the hardware calls for: legacy with `--low-gpu`, 4 GB of RAM
    /// or less, a software or downlevel GPU, or desktop OpenGL; balanced on
    /// integrated GPUs; high quality otherwise.
    pub(crate) fn auto_tier(&self) -> Tier {
        if self.low_gpu_requested || self.ram_bytes.is_some_and(|r| r <= LEGACY_RAM) {
            return Tier::Legacy;
        }
        match &self.gpu {
            Some(g) if g.device_type == wgpu::DeviceType::Cpu || g.max_texture_2d < DOWNLEVEL_TEXTURE || g.gl_fallback() => {
                Tier::Legacy
            }
            Some(g) if g.device_type == wgpu::DeviceType::IntegratedGpu => Tier::Balanced,
            _ => Tier::HighQuality,
        }
    }

    /// The tier for the profile picked in settings.
    pub(crate) fn tier(&self, choice: PerformanceProfile) -> Tier {
        match choice {
            PerformanceProfile::Auto => self.auto_tier(),
            PerformanceProfile::HighQuality => Tier::HighQuality,
            PerformanceProfile::Balanced => Tier::Balanced,
            PerformanceProfile::Legacy => Tier::Legacy,
        }
    }

    /// The GPU canvas settings for `tier`. In the legacy tier strokes
    /// accumulate in 8 bits where float targets blend slowly or not at all
    /// (desktop OpenGL). `PAINTER_LOW_VRAM` (`1`, `8bit` or `0`) still wins.
    pub(crate) fn render_profile(&self, tier: Tier) -> RenderProfile {
        let low_vram = std::env::var("PAINTER_LOW_VRAM").ok();
        self.render_profile_with(tier, low_vram.as_deref())
    }

    fn render_profile_with(&self, tier: Tier, low_vram: Option<&str>) -> RenderProfile {
        let profile = match tier {
            Tier::HighQuality => RenderProfile::default(),
            Tier::Balanced => RenderProfile::low_vram(),
            Tier::Legacy => RenderProfile {
                low_precision_strokes: self.gpu.as_ref().is_some_and(|g| !g.float_blend || g.gl_fallback()),
                ..RenderProfile::legacy()
            },
        };
        let compact_atlas = profile.compact_atlas;
        match low_vram {
            Some("0") => RenderProfile::default(),
            Some("8bit") => RenderProfile { low_precision_strokes: true, compact_atlas, ..RenderProfile::low_vram() },
            Some(v) if !v.is_empty() => RenderProfile { compact_atlas, ..RenderProfile::low_vram() },
            _ => profile,
        }
    }

    /// One line on the machine, for settings: RAM and GPU.
    pub(crate) fn summary(&self) -> String {
        let ram = match self.ram_bytes {
            Some(r) => format!("{:.1} GB RAM", r as f64 / GIB as f64),
            None => "RAM ?".to_string(),
        };
        match &self.gpu {
            Some(g) => format!("{ram} · {} ({:?}, {:?})", g.name, g.device_type, g.backend),
            None => ram,
        }
    }
}

/// How much RAM the undo history and the open documents may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RamBudget {
    /// Undo steps kept per document.
    pub(crate) undo_steps: usize,
    /// RAM one document's undo history may keep before paging steps to disk, in MiB.
    pub(crate) history_mib: usize,
    /// Minutes before an idle background tab is suspended.
    pub(crate) idle_minutes: u64,
    /// RAM all open documents may take before background tabs are suspended, in MiB.
    pub(crate) tabs_mib: u64,
}

impl RamBudget {
    /// The budgets of `tier`.
    pub(crate) fn of(tier: Tier) -> Self {
        match tier {
            Tier::HighQuality => {
                Self { undo_steps: crate::app::UNDO_LIMIT, history_mib: crate::app::HISTORY_RAM_MIB, idle_minutes: 5, tabs_mib: 2048 }
            }
            Tier::Balanced => Self { undo_steps: 60, history_mib: 256, idle_minutes: 3, tabs_mib: 1024 },
            Tier::Legacy => Self { undo_steps: 20, history_mib: 96, idle_minutes: 1, tabs_mib: 512 },
        }
    }

    /// The budgets in effect: the [`tier`]'s, or the legacy ones with
    /// `PAINTER_LOW_RAM=1` (`0`: the high-quality ones).
    pub(crate) fn current() -> Self {
        match std::env::var("PAINTER_LOW_RAM").as_deref() {
            Ok("0") => Self::of(Tier::HighQuality),
            Ok(v) if !v.is_empty() => Self::of(Tier::Legacy),
            _ => Self::of(tier()),
        }
    }
}

/// Physical memory installed, as the system reports it.
#[cfg(windows)]
fn total_ram_bytes() -> Option<u64> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    // SAFETY: `status` is a valid MEMORYSTATUSEX with its size filled in.
    unsafe { GlobalMemoryStatusEx(&mut status) }.ok()?;
    Some(status.ullTotalPhys)
}

/// Physical memory installed: `MemTotal` in /proc/meminfo.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn total_ram_bytes() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = meminfo.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kib: u64 = line.trim_start_matches("MemTotal:").trim().trim_end_matches("kB").trim().parse().ok()?;
    Some(kib * 1024)
}

/// Physical memory installed: the `hw.memsize` sysctl.
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn total_ram_bytes() -> Option<u64> {
    let mut bytes: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    // SAFETY: the name is NUL-terminated and `bytes` / `len` describe a u64 buffer.
    let ok = unsafe {
        libc::sysctlbyname(c"hw.memsize".as_ptr(), (&mut bytes as *mut u64).cast(), &mut len, std::ptr::null_mut(), 0) == 0
    };
    (ok && bytes > 0).then_some(bytes)
}

#[cfg(not(any(windows, target_os = "linux", target_os = "android", target_os = "macos", target_os = "ios")))]
fn total_ram_bytes() -> Option<u64> {
    None
}

impl PainterApp {
    /// Applies the Performance Profile picked in settings: the RAM budgets of
    /// the tab suspension and this document's history (other documents keep
    /// theirs until reopened) and, through the next canvas frame, the GPU
    /// canvas's targets and a rebuilt atlas.
    pub(crate) fn apply_performance_profile(&mut self) {
        let tier = self.hardware.tier(self.ws.chrome.performance);
        set_tier(tier);
        self.docs.suspend = crate::document_manager::session_store::SuspendPolicy::from_env();
        crate::app::apply_history_budget(&mut self.history);
        if self.gpu_ready && self.stroke.is_none() {
            self.pending_render_profile = Some(self.hardware.render_profile(tier));
            // A new epoch rebuilds the GPU canvas's atlas with the new page size.
            self.epoch += 1;
            self.resync_gpu();
        }
        self.status = format!("{} {}", tr("Performance profile:"), tr(tier.profile().label()));
    }

    /// The Performance Profile choice, with what Automatic picked on this machine.
    pub(crate) fn performance_ui(&mut self, ui: &mut eframe::egui::Ui) {
        let auto = self.hardware.auto_tier();
        let mut pick = None;
        for choice in PerformanceProfile::ALL {
            let label = match choice {
                PerformanceProfile::Auto => format!("{} ({})", tr(choice.label()), tr(auto.profile().label())),
                _ => tr(choice.label()).to_string(),
            };
            if ui.radio(self.ws.chrome.performance == choice, label).clicked() {
                pick = Some(choice);
            }
        }
        ui.separator();
        ui.small(self.hardware.summary());
        ui.small(tr("Legacy / Low Resource: smaller GPU targets and atlas, 8-bit strokes on old GPUs, less RAM for undo and tabs"));
        if let Some(choice) = pick {
            if choice != self.ws.chrome.performance {
                self.ws.chrome.performance = choice;
                self.apply_performance_profile();
            }
            ui.close_menu();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu(device_type: wgpu::DeviceType, backend: wgpu::Backend, max_texture_2d: u32, float_blend: bool) -> GpuInfo {
        GpuInfo { name: "test".into(), device_type, backend, max_texture_2d, float_blend }
    }

    fn machine(ram_gib: u64, gpu: Option<GpuInfo>) -> Hardware {
        Hardware { ram_bytes: Some(ram_gib * GIB), gpu, low_gpu_requested: false }
    }

    #[test]
    fn legacy_machines_are_detected() {
        let discrete = gpu(wgpu::DeviceType::DiscreteGpu, wgpu::Backend::Vulkan, 16384, true);
        assert_eq!(machine(16, Some(discrete.clone())).auto_tier(), Tier::HighQuality);
        // A ThinkPad X200: 4 GB, whatever the GPU.
        assert_eq!(machine(4, Some(discrete.clone())).auto_tier(), Tier::Legacy);
        assert_eq!(Hardware { low_gpu_requested: true, ..machine(16, Some(discrete)) }.auto_tier(), Tier::Legacy);
        assert_eq!(machine(16, Some(gpu(wgpu::DeviceType::Cpu, wgpu::Backend::Vulkan, 8192, true))).auto_tier(), Tier::Legacy);
        assert_eq!(machine(16, Some(gpu(wgpu::DeviceType::IntegratedGpu, wgpu::Backend::Dx12, 2048, true))).auto_tier(), Tier::Legacy);
        assert_eq!(machine(8, Some(gpu(wgpu::DeviceType::IntegratedGpu, wgpu::Backend::Dx12, 16384, true))).auto_tier(), Tier::Balanced);
        if !cfg!(any(target_os = "android", target_os = "ios")) {
            let gma = gpu(wgpu::DeviceType::IntegratedGpu, wgpu::Backend::Gl, 8192, true);
            assert_eq!(machine(8, Some(gma)).auto_tier(), Tier::Legacy);
        }
        // Unknown RAM and no GPU: nothing says it's old.
        assert_eq!(Hardware::default().auto_tier(), Tier::HighQuality);
    }

    #[test]
    fn the_choice_overrides_detection() {
        let old = machine(2, None);
        assert_eq!(old.tier(PerformanceProfile::Auto), Tier::Legacy);
        assert_eq!(old.tier(PerformanceProfile::HighQuality), Tier::HighQuality);
        assert_eq!(machine(32, None).tier(PerformanceProfile::Legacy), Tier::Legacy);
    }

    #[test]
    fn legacy_tier_shrinks_every_part() {
        let gl = machine(4, Some(gpu(wgpu::DeviceType::IntegratedGpu, wgpu::Backend::Gl, 8192, true)));
        let legacy = gl.render_profile_with(Tier::Legacy, None);
        assert!(legacy.viewport_compositing && legacy.clipped_strokes && legacy.compact_atlas);
        assert_eq!(legacy.low_precision_strokes, !cfg!(any(target_os = "android", target_os = "ios")));
        let vulkan = machine(4, Some(gpu(wgpu::DeviceType::IntegratedGpu, wgpu::Backend::Vulkan, 8192, true)));
        assert!(!vulkan.render_profile_with(Tier::Legacy, None).low_precision_strokes);
        let no_f16 = machine(4, Some(gpu(wgpu::DeviceType::IntegratedGpu, wgpu::Backend::Vulkan, 8192, false)));
        assert!(no_f16.render_profile_with(Tier::Legacy, None).low_precision_strokes);
        assert_eq!(vulkan.render_profile_with(Tier::HighQuality, None), RenderProfile::default());
        assert_eq!(vulkan.render_profile_with(Tier::Balanced, None), RenderProfile::low_vram());
        let budget = RamBudget::of(Tier::Legacy);
        assert_eq!((budget.undo_steps, budget.history_mib, budget.idle_minutes, budget.tabs_mib), (20, 96, 1, 512));
    }

    #[test]
    fn low_vram_variable_still_wins() {
        let hw = machine(16, Some(gpu(wgpu::DeviceType::DiscreteGpu, wgpu::Backend::Vulkan, 16384, true)));
        assert_eq!(hw.render_profile_with(Tier::HighQuality, Some("1")), RenderProfile::low_vram());
        let eight = hw.render_profile_with(Tier::Legacy, Some("8bit"));
        assert!(eight.low_precision_strokes && eight.compact_atlas);
        assert_eq!(hw.render_profile_with(Tier::Legacy, Some("0")), RenderProfile::default());
    }

    #[test]
    fn total_ram_is_reported_on_desktop() {
        if cfg!(any(windows, target_os = "linux", target_os = "macos")) {
            assert!(total_ram_bytes().is_some_and(|r| r > 256 << 20));
        }
    }
}
