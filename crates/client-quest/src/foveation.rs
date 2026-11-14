// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Fixed foveated rendering on the headset: the level asked for, what the runtime and the GPU offer for it, the density map Tomori makes where the runtime gives none, and which render passes a density map is attached to, so that `xr.rs` and `vulkan.rs` hold only the device calls. See `design/rendering.md`.
//!
//! wgpu does not expose Vulkan's fragment density map, so the device's `vkCreateRenderPass` and `vkCreateFramebuffer`, which wgpu-hal calls, are wrapped (`vulkan.rs`): each render pass shaped as an eye's is created with a density map attachment, and each of its framebuffers with the density map of the eye it draws. The map belongs to the whole pass, so the multisampled colour and depth, and the resolve into the runtime's image, are all drawn at its density.
use crate::scene::headset::Fov;
use ash::vk;
use openxr as xr;
use std::collections::{HashMap, HashSet};

/// How much less the edges of each eye's view are shaded than its middle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// Every pixel shaded, as without foveation.
    None,
    Low,
    Medium,
    High,
}

impl Level {
    /// High: on the Quest 3 it took a fifth off the eyes' GPU time where medium took nothing measurable, and the text looked at stayed sharp, since it is read where the eyes look; see `design/rendering.md`.
    pub const DEFAULT: Self = Self::High;

    /// The runtime's level for this one.
    pub fn xr(self) -> xr::FoveationLevelFB {
        match self {
            Self::None => xr::FoveationLevelFB::NONE,
            Self::Low => xr::FoveationLevelFB::LOW,
            Self::Medium => xr::FoveationLevelFB::MEDIUM,
            Self::High => xr::FoveationLevelFB::HIGH,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// The foveation asked for: a level, and whether Tomori's own density map is to be used even where the runtime gives one, to compare the two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub level: Level,
    pub own: bool,
}

impl Request {
    /// What `value` asks for, as the headset's `tomori.foveation` intent extra or the desktop's `TOMORI_FOVEATION`: `none` or `off`, `low`, `medium` or `high`, or their numbers 0 to 3, optionally followed by `:own`, as in `high:own`; [`Level::DEFAULT`] when absent or not understood.
    pub fn parse(value: Option<&str>) -> Self {
        let default = Self {
            level: Level::DEFAULT,
            own: false,
        };
        let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
            return default;
        };
        let lower = value.to_ascii_lowercase();
        let (level, own) = match lower.split_once(':') {
            Some((level, "own")) => (level, true),
            Some(_) => ("", false),
            None => (lower.as_str(), false),
        };
        let level = match level {
            "none" | "off" | "0" => Level::None,
            "low" | "1" => Level::Low,
            "medium" | "2" => Level::Medium,
            "high" | "3" => Level::High,
            _ => {
                log::warn!(
                    "Ignored tomori.foveation {value:?}: use none, low, medium or high, optionally followed by :own"
                );
                return default;
            }
        };
        Self { level, own }
    }
}

/// The runtime's foveation extensions, as it offers them or as Tomori enables them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Extensions {
    /// `XR_FB_foveation`: a swapchain made for foveation, and its profiles.
    pub foveation: bool,
    /// `XR_FB_foveation_configuration`: profiles by level.
    pub configuration: bool,
    /// `XR_FB_swapchain_update_state`: a profile applied to a swapchain.
    pub update_state: bool,
    /// `XR_FB_foveation_vulkan`: the density map of each of the swapchain's images.
    pub vulkan: bool,
}

impl Extensions {
    pub fn offered(supported: &xr::ExtensionSet) -> Self {
        Self {
            foveation: supported.fb_foveation,
            configuration: supported.fb_foveation_configuration,
            update_state: supported.fb_swapchain_update_state,
            vulkan: supported.fb_foveation_vulkan,
        }
    }

    /// Those to enable for `request`: all four where the runtime offers them all and a level is asked for, since its density maps need every one, and otherwise none.
    pub fn enabled(self, request: Request) -> Self {
        if request.level != Level::None
            && self.foveation
            && self.configuration
            && self.update_state
            && self.vulkan
        {
            self
        } else {
            Self::default()
        }
    }

    pub fn enable(self, set: &mut xr::ExtensionSet) {
        set.fb_foveation |= self.foveation;
        set.fb_foveation_configuration |= self.configuration;
        set.fb_swapchain_update_state |= self.update_state;
        set.fb_foveation_vulkan |= self.vulkan;
    }

    /// Whether the runtime's density maps can be asked for.
    pub fn maps(self) -> bool {
        self.foveation && self.configuration && self.update_state && self.vulkan
    }
}

/// What the GPU offers for drawing at a density map: `VK_EXT_fragment_density_map` with its `fragmentDensityMap` and `fragmentDensityMapNonSubsampledImages` features, since the images drawn are ordinary ones, and its `R8G8_UNORM` format usable as a density map.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Gpu {
    pub extension: bool,
    pub density_map: bool,
    pub non_subsampled_images: bool,
    pub format: bool,
}

impl Gpu {
    pub fn usable(self) -> bool {
        self.extension && self.density_map && self.non_subsampled_images && self.format
    }
}

/// Where the eyes' density map comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Map {
    /// The runtime's, for each swapchain image, from its profile at the level asked for.
    Runtime,
    /// Tomori's own, of the same shape for each level, where the runtime gives none or `:own` asks for it.
    Own,
}

/// How the eyes are foveated, decided before the swapchain is made: at which level, and whether the runtime's maps are asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub level: Level,
    /// Whether the swapchain is made for the runtime's density maps and given its profile.
    pub runtime: bool,
    own: bool,
}

impl Plan {
    /// The plan for `request` with what `gpu` and the runtime's `enabled` extensions offer, or why the eyes are not foveated. Tomori's own map is used only where the runtime has `XR_FB_foveation`, as Meta's does, so that other runtimes draw as before.
    pub fn new(request: Request, gpu: Gpu, enabled: Extensions) -> Result<Self, &'static str> {
        if request.level == Level::None {
            return Err("not asked for");
        }
        if !gpu.usable() {
            return Err("the GPU cannot draw at a fragment density map");
        }
        if !enabled.maps() {
            return Err("the runtime offers no foveation for Vulkan");
        }
        Ok(Self {
            level: request.level,
            runtime: !request.own,
            own: true,
        })
    }

    /// Where the map comes from once the swapchain's images are known: the runtime's where it `gave` one for every image.
    pub fn map(self, gave: bool) -> Option<Map> {
        if self.runtime && gave {
            Some(Map::Runtime)
        } else if self.own {
            Some(Map::Own)
        } else {
            None
        }
    }
}

/// The density at which every pixel is shaded.
pub const FULL: u8 = 255;
/// Just under a half, so that the fragment area, its inverse, is just over 2 and not clamped down to 1.
const HALF: u8 = 127;
/// Just under a quarter, for an area just over 4.
const QUARTER: u8 = 63;

/// The size of each of the density map's texels, in pixels, where the GPU allows it.
const TEXEL: u32 = 32;

/// The texels of Tomori's own density map for an eye of `width` by `height` pixels, with the GPU's least and greatest texel sizes: 32 pixels across and down where it allows, so that the GPU, which sizes each texel by the image over the map, a power of two, takes 32.
pub fn map_size(width: u32, height: u32, min: vk::Extent2D, max: vk::Extent2D) -> vk::Extent2D {
    let texel = |least: u32, most: u32| TEXEL.clamp(least.max(1), most.max(least).max(1));
    vk::Extent2D {
        width: width.div_ceil(texel(min.width, max.width)),
        height: height.div_ceil(texel(min.height, max.height)),
    }
}

/// The rings of Tomori's own map for `level`, from where the eye looks straight ahead out to its edges: the radius each reaches, as a fraction of the half-image, and its density across and down. Full in the middle, then half across, then half both ways, then a quarter both ways at the edges. High's are as measured on the headset; medium's and low's reach a little further out each, since a map whose full middle reached 0.55 and whose edges were a quarter across and a half down, though it shaded about half the pixels, saved nothing measurable: the GPU's own cost of drawing at a density map took what it saved, and only high's larger fragments at the edges saved more than that.
fn rings(level: Level) -> &'static [(f32, [u8; 2])] {
    match level {
        Level::None => &[(f32::INFINITY, [FULL, FULL])],
        Level::Low => &[
            (0.5, [FULL, FULL]),
            (0.75, [HALF, FULL]),
            (1.0, [HALF, HALF]),
            (f32::INFINITY, [QUARTER, QUARTER]),
        ],
        Level::Medium => &[
            (0.45, [FULL, FULL]),
            (0.7, [HALF, FULL]),
            (0.95, [HALF, HALF]),
            (f32::INFINITY, [QUARTER, QUARTER]),
        ],
        Level::High => &[
            (0.4, [FULL, FULL]),
            (0.65, [HALF, FULL]),
            (0.9, [HALF, HALF]),
            (f32::INFINITY, [QUARTER, QUARTER]),
        ],
    }
}

/// Where an eye with `fov` looks straight ahead in its image, from its left and top, as fractions of its width and height: off the middle towards the nose, and up, on the Quest 3.
pub fn centre(fov: Fov) -> [f32; 2] {
    let (left, right) = (fov.left.tan(), fov.right.tan());
    let (down, up) = (fov.down.tan(), fov.up.tan());
    [-left / (right - left), up / (up - down)]
}

/// Tomori's own density map for one eye, `size` texels, at `level`, its rings about `centre` (see [`centre`]), or full everywhere without one: two bytes a texel, `R8G8_UNORM`, the density across and down, row by row from the top.
pub fn own_map(level: Level, size: vk::Extent2D, centre: Option<[f32; 2]>) -> Vec<u8> {
    match centre {
        Some(centre) => map_of(rings(level), size, centre),
        None => map_of(rings(Level::None), size, [0.5, 0.5]),
    }
}

/// A density map of `size` texels in `rings` about `centre`, as [`own_map`].
fn map_of(rings: &[(f32, [u8; 2])], size: vk::Extent2D, [cx, cy]: [f32; 2]) -> Vec<u8> {
    let mut texels = Vec::with_capacity((size.width * size.height * 2) as usize);
    for y in 0..size.height {
        for x in 0..size.width {
            let u = (x as f32 + 0.5) / size.width as f32;
            let v = (y as f32 + 0.5) / size.height as f32;
            let radius = (2.0 * (u - cx)).hypot(2.0 * (v - cy));
            let (_, density) = rings
                .iter()
                .find(|(reach, _)| radius < *reach)
                .unwrap_or(&rings[rings.len() - 1]);
            texels.extend_from_slice(density);
        }
    }
    texels
}

/// The shape of the eyes' render pass, by which it is told from every other: one subpass, of one colour attachment in the eyes' format at their samples, and depth, drawing one view, or both eyes' by multiview, nothing else chained to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EyePass {
    pub format: vk::Format,
    pub samples: vk::SampleCountFlags,
}

impl EyePass {
    /// Whether `info` is shaped as the eyes' pass: those wgpu begins to draw an eye, and those it makes pipelines for that pass against, which must match it.
    ///
    /// # Safety
    ///
    /// `info`'s arrays must be valid for their counts, as `vkCreateRenderPass` requires.
    pub unsafe fn matches(self, info: &vk::RenderPassCreateInfo) -> bool {
        // SAFETY: the caller's.
        if !unsafe { eyes_chained(info.p_next) }
            || info.subpass_count != 1
            || info.p_subpasses.is_null()
        {
            return false;
        }
        // SAFETY: the caller's.
        let subpass = unsafe { &*info.p_subpasses };
        if subpass.color_attachment_count != 1 || subpass.p_depth_stencil_attachment.is_null() {
            return false;
        }
        // SAFETY: the caller's.
        let colour = unsafe { (*subpass.p_color_attachments).attachment };
        let attachments = unsafe { slice(info.p_attachments, info.attachment_count) };
        attachments
            .get(colour as usize)
            .is_some_and(|colour| colour.format == self.format && colour.samples == self.samples)
    }
}

/// The views both eyes are drawn in by multiview: the first two layers.
pub const BOTH: u32 = 0b11;

/// Whether `next`, what is chained to a render pass's create info, is nothing, or multiview alone, drawing both eyes in
/// its one subpass.
///
/// # Safety
///
/// `next` must be null or a valid chain of Vulkan structures.
unsafe fn eyes_chained(next: *const std::ffi::c_void) -> bool {
    if next.is_null() {
        return true;
    }
    // SAFETY: the caller's.
    let base = unsafe { &*next.cast::<vk::BaseInStructure>() };
    if base.s_type != vk::StructureType::RENDER_PASS_MULTIVIEW_CREATE_INFO || !base.p_next.is_null()
    {
        return false;
    }
    // SAFETY: as its type says.
    let multiview = unsafe { &*next.cast::<vk::RenderPassMultiviewCreateInfo>() };
    let masks = unsafe { slice(multiview.p_view_masks, multiview.subpass_count) };
    masks == [BOTH]
}

/// # Safety
///
/// `pointer` must be valid for `count` items, or null, or `count` zero: a null `pointer` is taken as no items, as Vulkan ignores an imageless framebuffer's.
pub(crate) unsafe fn slice<'a, T>(pointer: *const T, count: u32) -> &'a [T] {
    if count == 0 || pointer.is_null() {
        &[]
    } else {
        // SAFETY: the caller's.
        unsafe { std::slice::from_raw_parts(pointer, count as usize) }
    }
}

/// An eyes' render pass made with a density map: its attachments, the density map's last, and the create info chained to it. It holds what the create info points at, so it must outlive the call it is made for.
pub struct DensityPass<'a> {
    attachments: Vec<vk::AttachmentDescription>,
    density: vk::RenderPassFragmentDensityMapCreateInfoEXT<'a>,
    info: vk::RenderPassCreateInfo<'a>,
}

impl<'a> DensityPass<'a> {
    /// `info` with a density map attachment after its own, read in the layout the map is kept in.
    ///
    /// # Safety
    ///
    /// As [`EyePass::matches`].
    pub unsafe fn new(info: &vk::RenderPassCreateInfo<'a>) -> Box<Self> {
        // SAFETY: the caller's.
        let mut attachments = unsafe { slice(info.p_attachments, info.attachment_count) }.to_vec();
        let index = attachments.len() as u32;
        attachments.push(
            vk::AttachmentDescription::default()
                .format(vk::Format::R8G8_UNORM)
                .samples(vk::SampleCountFlags::TYPE_1)
                .load_op(vk::AttachmentLoadOp::LOAD)
                .store_op(vk::AttachmentStoreOp::DONT_CARE)
                .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
                .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
                .initial_layout(vk::ImageLayout::FRAGMENT_DENSITY_MAP_OPTIMAL_EXT)
                .final_layout(vk::ImageLayout::FRAGMENT_DENSITY_MAP_OPTIMAL_EXT),
        );
        // Boxed, so that the pointers into it stay where they are.
        let mut pass = Box::new(Self {
            attachments,
            density: vk::RenderPassFragmentDensityMapCreateInfoEXT::default()
                .fragment_density_map_attachment(vk::AttachmentReference {
                    attachment: index,
                    layout: vk::ImageLayout::FRAGMENT_DENSITY_MAP_OPTIMAL_EXT,
                }),
            info: *info,
        });
        pass.info.attachment_count = pass.attachments.len() as u32;
        pass.info.p_attachments = pass.attachments.as_ptr();
        pass.density.p_next = info.p_next;
        pass.info.p_next = (&raw const pass.density).cast();
        pass
    }

    pub fn info(&self) -> &vk::RenderPassCreateInfo<'a> {
        &self.info
    }
}

/// Which render passes are the eyes', and which density map each eye's framebuffer is given, as the wrapped device functions see them.
#[derive(Debug, Default)]
pub struct Passes {
    eye: Option<EyePass>,
    foveated: HashSet<vk::RenderPass>,
    /// Each eye's view of each swapchain image, with the view of its density map.
    targets: HashMap<vk::ImageView, vk::ImageView>,
    /// The density map for a framebuffer of the eyes' shape drawing anything else: full everywhere.
    full: vk::ImageView,
    /// Framebuffers given an eye's density map, and given the full one.
    pub framebuffers: u64,
    pub unknown: u64,
}

impl Passes {
    /// From now on, passes shaped as `eye` are made with a density map, `full` where a framebuffer draws no eye.
    pub fn foveate(&mut self, eye: EyePass, full: vk::ImageView) {
        self.eye = Some(eye);
        self.full = full;
    }

    /// Nothing more is foveated, and nothing about it kept.
    pub fn stop(&mut self) {
        *self = Self::default();
    }

    /// An eye drawn into `target` is drawn at `density`.
    pub fn target(&mut self, target: vk::ImageView, density: vk::ImageView) {
        self.targets.insert(target, density);
    }

    pub fn eye(&self) -> Option<EyePass> {
        self.eye
    }

    pub fn created(&mut self, pass: vk::RenderPass) {
        self.foveated.insert(pass);
    }

    pub fn destroyed(&mut self, pass: vk::RenderPass) {
        self.foveated.remove(&pass);
    }

    /// The density map to add to a framebuffer of `pass` with `attachments`: none unless `pass` was made with one, then the density map of the eye's image it draws, or the full one.
    pub fn density(
        &mut self,
        pass: vk::RenderPass,
        attachments: &[vk::ImageView],
    ) -> Option<vk::ImageView> {
        if !self.foveated.contains(&pass) {
            return None;
        }
        match attachments.iter().find_map(|view| self.targets.get(view)) {
            Some(density) => {
                self.framebuffers += 1;
                Some(*density)
            }
            None => {
                self.unknown += 1;
                Some(self.full)
            }
        }
    }
}

/// What the log says of foveation, beside the GPU's frame times.
pub fn summary(level: Level, map: Option<Map>) -> String {
    match map {
        None => "foveation off".to_owned(),
        Some(Map::Runtime) => format!("foveation {}, the runtime's map", level.name()),
        Some(Map::Own) => format!("foveation {}, Tomori's map", level.name()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ash::vk::Handle;

    #[test]
    fn a_level_is_asked_for_by_name_or_number() {
        let level = |value| Request::parse(Some(value)).level;
        assert_eq!(Request::parse(None).level, Level::High);
        assert_eq!(level(" "), Level::High);
        assert_eq!(level("off"), Level::None);
        assert_eq!(level("None"), Level::None);
        assert_eq!(level("0"), Level::None);
        assert_eq!(level("low"), Level::Low);
        assert_eq!(level("1"), Level::Low);
        assert_eq!(level("MEDIUM"), Level::Medium);
        assert_eq!(level("2"), Level::Medium);
        assert_eq!(level("high"), Level::High);
        assert_eq!(level("3"), Level::High);
        assert_eq!(level("highest"), Level::High);
        assert_eq!(
            Request::parse(Some("high:own")),
            Request {
                level: Level::High,
                own: true
            }
        );
        assert!(!Request::parse(Some("high")).own);
        // Anything else after the level is not understood.
        assert_eq!(
            Request::parse(Some("low:mine")),
            Request {
                level: Level::High,
                own: false
            }
        );
        for (level, xr, name) in [
            (Level::None, xr::FoveationLevelFB::NONE, "none"),
            (Level::Low, xr::FoveationLevelFB::LOW, "low"),
            (Level::Medium, xr::FoveationLevelFB::MEDIUM, "medium"),
            (Level::High, xr::FoveationLevelFB::HIGH, "high"),
        ] {
            assert_eq!(level.xr(), xr);
            assert_eq!(level.name(), name);
        }
    }

    fn offered() -> xr::ExtensionSet {
        let mut supported = xr::ExtensionSet::default();
        supported.fb_foveation = true;
        supported.fb_foveation_configuration = true;
        supported.fb_swapchain_update_state = true;
        supported.fb_foveation_vulkan = true;
        supported
    }

    const ASKED: Request = Request {
        level: Level::Medium,
        own: false,
    };

    const GPU: Gpu = Gpu {
        extension: true,
        density_map: true,
        non_subsampled_images: true,
        format: true,
    };

    #[test]
    fn the_runtime_s_four_extensions_are_enabled_together_or_not_at_all() {
        let all = Extensions::offered(&offered());
        assert!(all.maps());
        assert_eq!(all.enabled(ASKED), all);
        let mut set = xr::ExtensionSet::default();
        all.enabled(ASKED).enable(&mut set);
        assert!(set.fb_foveation && set.fb_foveation_configuration);
        assert!(set.fb_swapchain_update_state && set.fb_foveation_vulkan);
        // Not asked for: nothing is enabled.
        let none = Request {
            level: Level::None,
            own: false,
        };
        assert_eq!(all.enabled(none), Extensions::default());
        // Any one missing, as on a desktop runtime without them: nothing is enabled.
        for missing in 0..4 {
            let mut supported = offered();
            match missing {
                0 => supported.fb_foveation = false,
                1 => supported.fb_foveation_configuration = false,
                2 => supported.fb_swapchain_update_state = false,
                _ => supported.fb_foveation_vulkan = false,
            }
            let offered = Extensions::offered(&supported);
            assert!(!offered.maps());
            assert_eq!(offered.enabled(ASKED), Extensions::default());
            let mut set = xr::ExtensionSet::default();
            offered.enabled(ASKED).enable(&mut set);
            assert!(!set.fb_foveation && !set.fb_foveation_vulkan);
        }
    }

    #[test]
    fn the_eyes_are_foveated_only_where_the_runtime_and_the_gpu_both_can() {
        let enabled = Extensions::offered(&offered()).enabled(ASKED);
        let plan = Plan::new(ASKED, GPU, enabled).unwrap();
        assert_eq!(plan.level, Level::Medium);
        assert!(plan.runtime);
        // The runtime's map where it gives one, and Tomori's where it gives none.
        assert_eq!(plan.map(true), Some(Map::Runtime));
        assert_eq!(plan.map(false), Some(Map::Own));
        // Tomori's asked for, whatever the runtime gives.
        let own = Plan::new(Request { own: true, ..ASKED }, GPU, enabled).unwrap();
        assert!(!own.runtime);
        assert_eq!(own.map(true), Some(Map::Own));
        assert_eq!(Plan { own: false, ..plan }.map(false), None);
        // Not asked for, a GPU without density maps, and a runtime without foveation for Vulkan.
        let none = Request {
            level: Level::None,
            own: false,
        };
        assert_eq!(Plan::new(none, GPU, enabled), Err("not asked for"));
        for gpu in [
            Gpu::default(),
            Gpu {
                extension: false,
                ..GPU
            },
            Gpu {
                density_map: false,
                ..GPU
            },
            Gpu {
                non_subsampled_images: false,
                ..GPU
            },
            Gpu {
                format: false,
                ..GPU
            },
        ] {
            assert!(!gpu.usable());
            assert_eq!(
                Plan::new(ASKED, gpu, enabled),
                Err("the GPU cannot draw at a fragment density map")
            );
        }
        assert_eq!(
            Plan::new(ASKED, GPU, Extensions::default()),
            Err("the runtime offers no foveation for Vulkan")
        );
        assert_eq!(summary(Level::Medium, None), "foveation off");
        assert_eq!(
            summary(Level::Medium, Some(Map::Runtime)),
            "foveation medium, the runtime's map"
        );
        assert_eq!(
            summary(Level::High, Some(Map::Own)),
            "foveation high, Tomori's map"
        );
    }

    #[test]
    fn tomori_s_map_has_texels_of_32_pixels_where_the_gpu_allows() {
        let extent = |width, height| vk::Extent2D { width, height };
        // The Quest 3's eye, at 32 pixels a texel: the GPU, dividing the image by the map, takes 32.
        let size = map_size(1680, 1760, extent(1, 1), extent(32, 32));
        assert_eq!(size, extent(53, 55));
        for (pixels, texels) in [(1680, size.width), (1760, size.height)] {
            assert_eq!((pixels / texels).next_power_of_two(), 32);
        }
        // A GPU whose texels are larger, or smaller, at most.
        assert_eq!(
            map_size(1680, 1760, extent(64, 64), extent(128, 128)),
            extent(27, 28)
        );
        assert_eq!(
            map_size(1680, 1760, extent(8, 8), extent(16, 16)),
            extent(105, 110)
        );
        // A GPU that reports nothing is taken as texels of a pixel, at least.
        assert_eq!(map_size(10, 10, extent(0, 0), extent(0, 0)), extent(10, 10));
    }

    fn quest() -> Fov {
        // The Quest 3's left eye, about: 52° out, 41° in, 40° up and 50° down.
        Fov {
            left: -52f32.to_radians(),
            right: 41f32.to_radians(),
            down: -50f32.to_radians(),
            up: 40f32.to_radians(),
        }
    }

    #[test]
    fn an_eye_looks_straight_ahead_towards_the_nose_and_up() {
        let [x, y] = centre(quest());
        assert!((0.55..0.65).contains(&x), "{x}");
        assert!((0.35..0.45).contains(&y), "{y}");
        let symmetric = Fov {
            left: -0.8,
            right: 0.8,
            down: -0.7,
            up: 0.7,
        };
        assert_eq!(centre(symmetric), [0.5, 0.5]);
    }

    #[test]
    fn tomori_s_map_is_full_in_the_middle_and_less_towards_the_edges() {
        let size = vk::Extent2D {
            width: 53,
            height: 55,
        };
        let centre = centre(quest());
        let at = |map: &[u8], u: f32, v: f32| {
            let (x, y) = ((u * 53.0) as usize, (v * 55.0) as usize);
            let index = 2 * (y * 53 + x);
            [map[index], map[index + 1]]
        };
        for level in [Level::None, Level::Low, Level::Medium, Level::High] {
            // Without an eye, everything is shaded.
            let map = own_map(level, size, None);
            assert_eq!(map.len(), 53 * 55 * 2);
            assert!(map.iter().all(|density| *density == FULL));
            let map = own_map(level, size, Some(centre));
            assert_eq!(map.len(), 53 * 55 * 2);
            // Where the eye looks straight ahead, and a little to either side, every pixel is shaded.
            assert_eq!(at(&map, centre[0], centre[1]), [FULL, FULL]);
            assert_eq!(at(&map, centre[0] - 0.15, centre[1]), [FULL, FULL]);
            assert_eq!(at(&map, centre[0] + 0.15, centre[1] + 0.1), [FULL, FULL]);
            // Every density is one the GPU takes as a whole fragment area: 1, just over 2 or just over 4.
            assert!(
                map.iter()
                    .all(|density| [FULL, HALF, QUARTER].contains(density))
            );
        }
        let corner = |level| at(&own_map(level, size, Some(centre)), 0.0, 0.999);
        assert_eq!(corner(Level::None), [FULL, FULL]);
        for level in [Level::Low, Level::Medium, Level::High] {
            assert_eq!(corner(level), [QUARTER, QUARTER]);
        }
        // Each level shades less than the one before.
        let shade = |map: &[u8]| -> f32 {
            map.chunks(2)
                .map(|texel| texel[0] as f32 * texel[1] as f32 / (255.0 * 255.0))
                .sum()
        };
        let shaded = |level| shade(&own_map(level, size, Some(centre)));
        let levels = [Level::None, Level::Low, Level::Medium, Level::High].map(shaded);
        assert!(
            levels.windows(2).all(|pair| pair[1] < pair[0]),
            "{levels:?}"
        );
        // Low and medium shade less than the medium first measured on the headset, which saved nothing.
        let measured_nothing = map_of(
            &[
                (0.55, [FULL, FULL]),
                (0.8, [HALF, FULL]),
                (1.05, [HALF, HALF]),
                (f32::INFINITY, [QUARTER, HALF]),
            ],
            size,
            centre,
        );
        let measured_nothing: f32 = shade(&measured_nothing);
        assert!(
            levels[1] < measured_nothing,
            "{levels:?} {measured_nothing}"
        );
    }

    fn eye_pass(
        attachments: &[vk::AttachmentDescription],
        colours: &[vk::AttachmentReference],
        resolves: &[vk::AttachmentReference],
        depth: Option<&vk::AttachmentReference>,
    ) -> bool {
        // As wgpu-hal does: the resolves share the colours' count, so a pass without them has none.
        let mut subpass = vk::SubpassDescription::default().color_attachments(colours);
        if !resolves.is_empty() {
            subpass = subpass.resolve_attachments(resolves);
        }
        if let Some(depth) = depth {
            subpass = subpass.depth_stencil_attachment(depth);
        }
        let subpasses = [subpass];
        let info = vk::RenderPassCreateInfo::default()
            .attachments(attachments)
            .subpasses(&subpasses);
        unsafe { EYES.matches(&info) }
    }

    const EYES: EyePass = EyePass {
        format: vk::Format::R8G8B8A8_SRGB,
        samples: vk::SampleCountFlags::TYPE_4,
    };

    fn attachment(format: vk::Format, samples: vk::SampleCountFlags) -> vk::AttachmentDescription {
        vk::AttachmentDescription::default()
            .format(format)
            .samples(samples)
    }

    fn reference(attachment: u32) -> vk::AttachmentReference {
        vk::AttachmentReference {
            attachment,
            layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        }
    }

    #[test]
    fn only_render_passes_shaped_as_the_eyes_are_foveated() {
        let colour = attachment(vk::Format::R8G8B8A8_SRGB, vk::SampleCountFlags::TYPE_4);
        let resolve = attachment(vk::Format::R8G8B8A8_SRGB, vk::SampleCountFlags::TYPE_1);
        let depth = attachment(vk::Format::D32_SFLOAT, vk::SampleCountFlags::TYPE_4);
        // The eye's pass, resolving into the swapchain's image, and the pass its pipelines are made against.
        let attachments = [colour, resolve, depth];
        assert!(eye_pass(
            &attachments,
            &[reference(0)],
            &[reference(1)],
            Some(&reference(2))
        ));
        assert!(eye_pass(
            &[colour, depth],
            &[reference(0)],
            &[],
            Some(&reference(1))
        ));
        // No depth, as wgpu clears a texture: not the eye's.
        assert!(!eye_pass(&[colour], &[reference(0)], &[], None));
        // Another format or sample count, two colours, a colour unused or out of range, or no colour.
        let other = attachment(vk::Format::R8G8B8A8_UNORM, vk::SampleCountFlags::TYPE_4);
        assert!(!eye_pass(
            &[other, depth],
            &[reference(0)],
            &[],
            Some(&reference(1))
        ));
        assert!(!eye_pass(
            &[resolve, depth],
            &[reference(0)],
            &[],
            Some(&reference(1))
        ));
        assert!(!eye_pass(
            &[colour, colour, depth],
            &[reference(0), reference(1)],
            &[],
            Some(&reference(2))
        ));
        let unused = reference(vk::ATTACHMENT_UNUSED);
        assert!(!eye_pass(
            &[colour, depth],
            &[unused],
            &[],
            Some(&reference(1))
        ));
        assert!(!eye_pass(
            &[colour, depth],
            &[reference(5)],
            &[],
            Some(&reference(1))
        ));
        assert!(!eye_pass(&[depth], &[], &[], Some(&reference(0))));
        assert!(!eye_pass(&[], &[], &[], Some(&reference(0))));
        // Both eyes drawn by multiview, as wgpu chains it; but not other views, multiview with anything more chained,
        // anything else chained, or not one subpass.
        let (colours, depth_reference) = ([reference(0)], reference(1));
        let subpass = vk::SubpassDescription::default()
            .color_attachments(&colours)
            .depth_stencil_attachment(&depth_reference);
        let subpasses = [subpass, subpass];
        let both = [colour, depth];
        let chained = |masks: &[u32], more: bool| {
            let mut multiview = vk::RenderPassMultiviewCreateInfo::default()
                .view_masks(masks)
                .correlation_masks(masks);
            let mut density = vk::RenderPassFragmentDensityMapCreateInfoEXT::default();
            let mut info = vk::RenderPassCreateInfo::default()
                .attachments(&both)
                .subpasses(&subpasses[..1]);
            if more {
                info = info.push_next(&mut density);
            }
            let info = info.push_next(&mut multiview);
            unsafe { EYES.matches(&info) }
        };
        assert!(chained(&[BOTH], false));
        assert!(!chained(&[0b1], false));
        assert!(!chained(&[BOTH, BOTH], false));
        assert!(!chained(&[], false));
        assert!(!chained(&[BOTH], true));
        let mut density = vk::RenderPassFragmentDensityMapCreateInfoEXT::default();
        let other = vk::RenderPassCreateInfo::default()
            .attachments(&both)
            .subpasses(&subpasses[..1])
            .push_next(&mut density);
        assert!(!unsafe { EYES.matches(&other) });
        let two = vk::RenderPassCreateInfo::default()
            .attachments(&both)
            .subpasses(&subpasses);
        assert!(!unsafe { EYES.matches(&two) });
        let none = vk::RenderPassCreateInfo::default().attachments(&both);
        assert!(!unsafe { EYES.matches(&none) });
    }

    #[test]
    fn an_eye_s_pass_is_made_with_its_density_map_last() {
        let colour = attachment(vk::Format::R8G8B8A8_SRGB, vk::SampleCountFlags::TYPE_4);
        let depth = attachment(vk::Format::D32_SFLOAT, vk::SampleCountFlags::TYPE_4);
        let attachments = [colour, depth];
        let colours = [reference(0)];
        let depth_reference = reference(1);
        let subpasses = [vk::SubpassDescription::default()
            .color_attachments(&colours)
            .depth_stencil_attachment(&depth_reference)];
        let info = vk::RenderPassCreateInfo::default()
            .attachments(&attachments)
            .subpasses(&subpasses);
        let pass = unsafe { DensityPass::new(&info) };
        let made = pass.info();
        assert_eq!(made.attachment_count, 3);
        assert_eq!(made.subpass_count, 1);
        assert_eq!(made.p_subpasses, info.p_subpasses);
        let made_attachments = unsafe { slice(made.p_attachments, made.attachment_count) };
        for (made, given) in made_attachments.iter().zip(&attachments) {
            assert_eq!((made.format, made.samples), (given.format, given.samples));
        }
        let density = made_attachments[2];
        assert_eq!(density.format, vk::Format::R8G8_UNORM);
        assert_eq!(density.samples, vk::SampleCountFlags::TYPE_1);
        assert_eq!(
            density.initial_layout,
            vk::ImageLayout::FRAGMENT_DENSITY_MAP_OPTIMAL_EXT
        );
        assert_eq!(density.final_layout, density.initial_layout);
        let chained = unsafe {
            &*made
                .p_next
                .cast::<vk::RenderPassFragmentDensityMapCreateInfoEXT>()
        };
        assert_eq!(
            chained.s_type,
            vk::StructureType::RENDER_PASS_FRAGMENT_DENSITY_MAP_CREATE_INFO_EXT
        );
        assert_eq!(chained.fragment_density_map_attachment.attachment, 2);
        assert!(chained.p_next.is_null());
        // A pass made with a density map is no longer shaped as the eyes'.
        assert!(!unsafe { EYES.matches(made) });
        // One drawing both eyes by multiview keeps multiview, chained after its density map.
        let masks = [BOTH];
        let mut multiview = vk::RenderPassMultiviewCreateInfo::default()
            .view_masks(&masks)
            .correlation_masks(&masks);
        let both = info.push_next(&mut multiview);
        assert!(unsafe { EYES.matches(&both) });
        let pass = unsafe { DensityPass::new(&both) };
        let chained = unsafe {
            &*pass
                .info()
                .p_next
                .cast::<vk::RenderPassFragmentDensityMapCreateInfoEXT>()
        };
        assert_eq!(chained.p_next, both.p_next);
        assert!(!unsafe { EYES.matches(pass.info()) });
        assert!(unsafe { slice::<u8>(std::ptr::null(), 0) }.is_empty());
        // An imageless framebuffer's attachments are null, whatever their count.
        assert!(unsafe { slice::<u8>(std::ptr::null(), 3) }.is_empty());
    }

    #[test]
    fn each_eye_s_framebuffer_is_given_the_density_map_of_the_image_it_draws() {
        let handle = |raw| vk::ImageView::from_raw(raw);
        let (pass, other) = (vk::RenderPass::from_raw(1), vk::RenderPass::from_raw(2));
        let mut passes = Passes::default();
        assert_eq!(passes.eye(), None);
        passes.foveate(EYES, handle(100));
        assert_eq!(passes.eye(), Some(EYES));
        passes.created(pass);
        passes.target(handle(10), handle(110));
        passes.target(handle(11), handle(111));
        // The eye's samples, its swapchain image's layer and depth.
        assert_eq!(
            passes.density(pass, &[handle(1), handle(11), handle(2)]),
            Some(handle(111))
        );
        assert_eq!(
            passes.density(pass, &[handle(1), handle(10), handle(2)]),
            Some(handle(110))
        );
        // A pass made without one is given none.
        assert_eq!(passes.density(other, &[handle(1), handle(10)]), None);
        // A pass of the eyes' shape drawing something else is given the full map.
        assert_eq!(
            passes.density(pass, &[handle(3), handle(4)]),
            Some(handle(100))
        );
        assert_eq!((passes.framebuffers, passes.unknown), (2, 1));
        passes.destroyed(pass);
        assert_eq!(passes.density(pass, &[handle(10)]), None);
        passes.stop();
        assert_eq!(passes.eye(), None);
        assert_eq!((passes.framebuffers, passes.unknown), (0, 0));
    }
}
