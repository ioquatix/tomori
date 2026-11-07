// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Scenery drawn natively in each eye's pass, and the sceneries a client can draw, by the identifiers that spaces name their environments by. See `design/environments.md`.
//!
//! The Minimal grid is built into the [`Renderer`](crate::graphics::Renderer), and is what a client shows for an environment it cannot draw. Every other environment's client half registers its scenery through [`Sceneries`] from its own crate, such as `tomori-environment-sky-client`, and an application registers the environments it presents.
use std::sync::Arc;

use glam::{Mat4, Vec3};

use crate::{environment::Environment, locomotion::Locomotion};

/// What a scenery is given when the renderer creates it.
pub struct Target<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub format: wgpu::TextureFormat,
    pub width: u32,
    pub height: u32,
    pub samples: u32,
}

/// Scenery drawn natively in each eye's pass.
pub trait Scenery {
    /// Once a frame, before the eyes: what the environment's client side shows now, the environment's own, which only
    /// its own scenery reads, such as where its animated models stand. See `design/environments.md`.
    fn animate(
        &mut self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _drawn: &(dyn std::any::Any + Send + Sync),
    ) {
    }
    /// Before the eye's pass: per-eye uniforms, for the eye's view and projection.
    fn prepare(&self, queue: &wgpu::Queue, eye: usize, scene_to_clip: Mat4);
    /// Before the eye's pass: work in its own passes, such as Sky's clouds.
    fn encode(&self, _encoder: &mut wgpu::CommandEncoder, _eye: usize) {}
    /// In the eye's pass, before the scene.
    fn draw(&self, pass: &mut wgpu::RenderPass<'_>, eye: usize);
    /// In the eye's pass, after the scene and before the panels.
    fn draw_translucent(&self, _pass: &mut wgpu::RenderPass<'_>, _eye: usize) {}
    /// While a space's scenery grows out of it, only inside this sphere (centre, radius).
    fn set_reveal(&mut self, reveal: Option<(Vec3, f32)>);
    /// The eyes' targets changed size.
    fn resize(&mut self, _device: &wgpu::Device, _width: u32, _height: u32) {}
}

/// Makes a scenery for a renderer's target.
type Create = Arc<dyn Fn(&Target) -> Box<dyn Scenery> + Send + Sync>;

/// The sceneries a client can draw, by the identifiers spaces name them by, in the order they were registered, with the locomotion each environment allows, and the environments it presents over the grid without a scenery of their own. The Minimal grid is built in, and is not registered. It starts empty: an application registers the environments it presents, each through its own crate, such as Sky's `tomori_environment_sky_client::register`. See `design/environments.md`.
#[derive(Clone, Default)]
pub struct Sceneries {
    entries: Vec<(Environment, Create)>,
    locomotion: Vec<(Environment, Locomotion)>,
    over_grid: Vec<Environment>,
    client_sides: Vec<(Environment, tomori_scene::client_side::Factory)>,
    licenses: Vec<(&'static str, &'static str)>,
}

impl Sceneries {
    /// None but the built-in grid, as [`Sceneries::default`] is.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Draw `create`'s scenery for the environment `id`, at protocol version 1, in place of any registered before under the same identifier.
    pub fn register(
        &mut self,
        id: &'static str,
        create: impl Fn(&Target) -> Box<dyn Scenery> + Send + Sync + 'static,
    ) {
        self.register_versioned(id, 1, create);
    }

    /// Draw `create`'s scenery for the environment `id` at protocol `version`, in place of any registered before under the same identifier, whatever its version. The grid, [`tomori_protocol::GRID`], is built in, at [`tomori_protocol::GRID_VERSION`]: a scenery registered for it is drawn with it, at the grid's version whatever version it is registered at. A client presents the environment only for a space whose environment is at the same version, and shows the grid for one at another; see `design/environments.md`.
    pub fn register_versioned(
        &mut self,
        id: &'static str,
        version: u32,
        create: impl Fn(&Target) -> Box<dyn Scenery> + Send + Sync + 'static,
    ) {
        let environment = Environment::new(id).with_version(version);
        self.entries
            .retain(|(registered, _)| *registered != environment);
        self.entries.push((environment, Arc::new(create)));
    }

    /// Present the environment `id` at protocol `version` over the grid, without a scenery of its own, in place of any presented so before under the same identifier: a client lists it in its `Hello` and shows a space in it without a notice, drawing the grid and whatever the shared scene presents of it, as the GitHub prototype's cards. A scenery registered for the same identifier is drawn instead, at its own version. See `design/environments.md`.
    pub fn present_over_grid(&mut self, id: &'static str, version: u32) {
        let environment = Environment::new(id).with_version(version);
        self.over_grid.retain(|presented| *presented != environment);
        self.over_grid.push(environment);
    }

    /// The environments presented over the grid, in the order they were presented, for the conversation.
    pub fn over_grid(&self) -> Vec<Environment> {
        self.over_grid.clone()
    }

    /// Let participants travel as `locomotion` says in a space bound to the environment `id`, when its scenery is drawn, in place of any declared before for it. An environment declared nothing has none. See `design/locomotion.md`.
    ///
    /// Provisional: once environments have server sides (#75), the profile is to come from the environment's server side instead, and this may be deprecated.
    pub fn set_locomotion(&mut self, id: &'static str, locomotion: Locomotion) {
        let environment = Environment::new(id);
        self.locomotion
            .retain(|(declared, _)| *declared != environment);
        self.locomotion.push((environment, locomotion));
    }

    /// Present the environment `id`, where this build draws its scenery, with the client side `create` makes for each
    /// space bound to it, in place of any registered before for it: what it shows beyond its scenery, from what its server
    /// side shares, such as its animated models, what the ray points at in it, and its sounds. See
    /// `design/environments.md`.
    pub fn set_client_side(
        &mut self,
        id: &'static str,
        create: impl Fn(&str) -> Box<dyn tomori_scene::client_side::ClientSide> + Send + Sync + 'static,
    ) {
        let environment = Environment::new(id);
        self.client_sides
            .retain(|(declared, _)| *declared != environment);
        self.client_sides.push((environment, Arc::new(create)));
    }

    /// The client sides registered for each environment, for the conversation.
    pub fn client_sides(&self) -> Vec<(Environment, tomori_scene::client_side::Factory)> {
        self.client_sides.clone()
    }

    /// The locomotion declared for each environment, for the conversation.
    pub fn locomotion(&self) -> Vec<(Environment, Locomotion)> {
        self.locomotion.clone()
    }

    /// Carry the licence notice `text` of what a registered environment bundles, such as its models, under the heading
    /// `what`, in place of any added before under the same heading, so that an application shows it with its own
    /// bundled assets' notices, as `tomori --licenses` does. An environment's own `register` adds its notices with
    /// its scenery.
    pub fn add_license(&mut self, what: &'static str, text: &'static str) {
        self.licenses.retain(|(added, _)| *added != what);
        self.licenses.push((what, text));
    }

    /// The licence notices added, each as its heading and its text, in the order they were added.
    pub fn licenses(&self) -> Vec<(&'static str, &'static str)> {
        self.licenses.clone()
    }

    /// Whether the environment `id` has a scenery here; the grid's, `minimal`, always does.
    pub fn contains(&self, id: &str) -> bool {
        self.environments()
            .iter()
            .any(|environment| environment.id() == id)
    }

    /// Every environment a client with these sceneries can draw, each at the version it was registered at: the grid, always at its built-in version, then the registered ones in order.
    pub fn environments(&self) -> Vec<Environment> {
        std::iter::once(Environment::MINIMAL)
            .chain(
                self.entries
                    .iter()
                    .map(|(environment, _)| *environment)
                    .filter(|environment| *environment != Environment::MINIMAL),
            )
            .collect()
    }

    /// Make each registered scenery for `target`.
    pub fn create(&self, target: &Target) -> Vec<(Environment, Box<dyn Scenery>)> {
        self.entries
            .iter()
            .map(|(environment, create)| (*environment, create(target)))
            .collect()
    }
}

/// A scenery for tests that check that a client draws the sceneries it is given, shared with the clients' tests through the `test-gpu` feature.
#[cfg(any(test, feature = "test-gpu"))]
pub mod test {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A scenery that draws nothing, and counts how many renderers created it and how many eyes it was drawn in.
    #[derive(Clone, Default)]
    pub struct Counted {
        created: Arc<AtomicUsize>,
        drawn: Arc<AtomicUsize>,
    }

    impl Counted {
        /// Sceneries with this one alone, for the environment `id`, so that a renderer given any others never creates it.
        pub fn sceneries(&self, id: &'static str) -> Sceneries {
            let mut sceneries = Sceneries::empty();
            let counted = self.clone();
            sceneries.register(id, move |_| counted.create());
            sceneries
        }

        /// This scenery, counted as created once more.
        pub fn create(&self) -> Box<dyn Scenery> {
            self.created.fetch_add(1, Ordering::Relaxed);
            Box::new(self.clone())
        }

        /// How many times a renderer created it.
        pub fn created(&self) -> usize {
            self.created.load(Ordering::Relaxed)
        }

        /// How many eyes it was drawn in.
        pub fn drawn(&self) -> usize {
            self.drawn.load(Ordering::Relaxed)
        }
    }

    /// A scenery that draws nothing, for registering where what is drawn does not matter.
    pub fn blank(_target: &Target) -> Box<dyn Scenery> {
        Box::new(Counted::default())
    }

    impl Scenery for Counted {
        fn prepare(&self, _queue: &wgpu::Queue, _eye: usize, _scene_to_clip: Mat4) {}

        fn draw(&self, _pass: &mut wgpu::RenderPass<'_>, _eye: usize) {
            self.drawn.fetch_add(1, Ordering::Relaxed);
        }

        fn set_reveal(&mut self, _reveal: Option<(Vec3, f32)>) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client side that shows nothing.
    struct Empty;

    impl tomori_scene::client_side::ClientSide for Empty {
        fn pick(
            &self,
            _ray: &tomori_scene::pointing::Ray,
        ) -> Option<tomori_scene::client_side::Picked> {
            None
        }

        fn update(
            &mut self,
            _input: tomori_scene::client_side::Input<'_>,
        ) -> tomori_scene::client_side::Output {
            tomori_scene::client_side::Output::new()
        }
    }

    #[test]
    fn client_sides_are_registered_by_identifier_and_none_by_default() {
        assert!(Sceneries::default().client_sides().is_empty());
        let mut sceneries = Sceneries::empty();
        let empty = |_: &str| Box::new(Empty) as Box<dyn tomori_scene::client_side::ClientSide>;
        sceneries.set_client_side("floor", empty);
        sceneries.set_client_side("deck", empty);
        // Registering an identifier again replaces it, at the end.
        sceneries.set_client_side("floor", empty);
        let registered = sceneries.client_sides();
        let ids: Vec<&str> = registered
            .iter()
            .map(|(environment, _)| environment.id())
            .collect();
        assert_eq!(ids, ["deck", "floor"]);
        assert!(
            (registered[0].1)("space-1")
                .pick(&tomori_scene::pointing::Ray::new(Vec3::ZERO, Vec3::Z).unwrap())
                .is_none()
        );
        let mut made = (registered[1].1)("space-1");
        assert!(
            made.pick(&tomori_scene::pointing::Ray::new(Vec3::ZERO, Vec3::Z).unwrap())
                .is_none()
        );
        assert!(
            made.update(tomori_scene::client_side::Input::new(
                Vec::new(),
                std::time::Instant::now()
            ))
            .drawn
            .is_none()
        );
    }

    #[test]
    fn licence_notices_are_kept_in_order_by_heading_and_none_by_default() {
        assert!(Sceneries::default().licenses().is_empty());
        let mut sceneries = Sceneries::empty();
        sceneries.add_license("Floor tiles", "CC0");
        sceneries.add_license("Deck", "MIT");
        // Adding a heading again replaces its text, at the end.
        sceneries.add_license("Floor tiles", "CC BY 4.0");
        assert_eq!(
            sceneries.licenses(),
            [("Deck", "MIT"), ("Floor tiles", "CC BY 4.0")]
        );
    }

    #[test]
    fn locomotion_is_declared_by_identifier_and_none_by_default() {
        use crate::locomotion::Surface;
        assert!(Sceneries::default().locomotion().is_empty());
        let mut sceneries = Sceneries::empty();
        let teleport = Locomotion::teleport(vec![Surface::disc(Vec3::ZERO, 3.0)]);
        sceneries.set_locomotion("floor", Locomotion::fly(Vec::new(), 10.0));
        sceneries.set_locomotion("deck", teleport.clone());
        // Declaring an identifier again replaces it, at the end.
        sceneries.set_locomotion("floor", teleport.clone());
        assert_eq!(
            sceneries.locomotion(),
            [
                (Environment::new("deck"), teleport.clone()),
                (Environment::new("floor"), teleport)
            ]
        );
    }

    #[test]
    fn the_grid_is_built_in_and_sceneries_are_kept_in_order_by_identifier() {
        let mut sceneries = Sceneries::empty();
        assert_eq!(sceneries.environments(), [Environment::MINIMAL]);
        // Nothing but the grid is registered by default: an application registers its environments.
        assert_eq!(Sceneries::default().environments(), [Environment::MINIMAL]);
        assert!(Sceneries::default().over_grid().is_empty());
        assert!(sceneries.contains("minimal"));
        assert!(!sceneries.contains("deck"));
        sceneries.register("floor", test::blank);
        sceneries.register("deck", test::blank);
        // Registering an identifier again replaces it, at the end.
        sceneries.register("floor", test::blank);
        assert_eq!(
            sceneries.environments(),
            [
                Environment::MINIMAL,
                Environment::new("deck"),
                Environment::new("floor")
            ]
        );
        // A scenery for the grid is drawn with it, and the grid is listed once.
        sceneries.register("minimal", test::blank);
        assert_eq!(sceneries.environments().len(), 3);
        // Each is at version 1 unless registered at another, which registering again replaces.
        let versions = |sceneries: &Sceneries| -> Vec<(&str, u32)> {
            sceneries
                .environments()
                .into_iter()
                .map(|environment| (environment.id(), environment.version()))
                .collect()
        };
        sceneries.register_versioned("floor", 2, test::blank);
        // The grid is built in at its own version, whatever version a scenery drawn with it is registered at.
        sceneries.register_versioned("minimal", 3, test::blank);
        assert_eq!(
            versions(&sceneries),
            [
                (tomori_protocol::GRID, tomori_protocol::GRID_VERSION),
                ("deck", 1),
                ("floor", 2)
            ]
        );
        sceneries.register("floor", test::blank);
        assert_eq!(sceneries.environments()[2].version(), 1);
    }

    #[test]
    fn environments_presented_over_the_grid_are_kept_in_order_by_identifier() {
        let mut sceneries = Sceneries::empty();
        sceneries.present_over_grid("board", 1);
        sceneries.present_over_grid("inbox", 2);
        // Presenting an identifier again replaces it, at the end.
        sceneries.present_over_grid("board", 3);
        let presented: Vec<(&str, u32)> = sceneries
            .over_grid()
            .into_iter()
            .map(|environment| (environment.id(), environment.version()))
            .collect();
        assert_eq!(presented, [("inbox", 2), ("board", 3)]);
        // They have no scenery of their own.
        assert_eq!(sceneries.environments(), [Environment::MINIMAL]);
    }

    #[test]
    fn a_renderer_creates_each_scenery_it_is_given_and_draws_the_current_environments() {
        use crate::graphics::Renderer;
        let counted = test::Counted::default();
        let floor = Environment::new("floor");
        crate::test_gpu::with(|crate::test_gpu::Gpu { device, queue, .. }| {
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let mut renderer = Renderer::with_sceneries(
                &device,
                &queue,
                format,
                8,
                8,
                1,
                &counted.sceneries("floor"),
            );
            assert_eq!(counted.created(), 1);
            assert_eq!(renderer.environments(), [Environment::MINIMAL, floor]);
            let views: Vec<_> = (0..2)
                .map(|_| {
                    device
                        .create_texture(&wgpu::TextureDescriptor {
                            label: Some("counted scenery eye"),
                            size: wgpu::Extent3d {
                                width: 8,
                                height: 8,
                                depth_or_array_layers: 1,
                            },
                            mip_level_count: 1,
                            sample_count: 1,
                            dimension: wgpu::TextureDimension::D2,
                            format,
                            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                            view_formats: &[],
                        })
                        .create_view(&Default::default())
                })
                .collect();
            let render = |renderer: &Renderer| {
                renderer.render(
                    &device,
                    &queue,
                    &views,
                    &[Mat4::IDENTITY; 2],
                    Mat4::IDENTITY,
                    &[],
                );
            };
            // The grid has no scenery of its own.
            render(&renderer);
            assert_eq!(counted.drawn(), 0);
            renderer.set_scenery(crate::arrival_motion::Scenery {
                environment: floor,
                reveal: Some(crate::arrival_motion::Sphere {
                    centre: Vec3::ZERO,
                    radius: 1.0,
                }),
            });
            render(&renderer);
            assert_eq!(counted.drawn(), 2);
            assert_eq!(renderer.environment(), floor);
            // A blank scenery is made for a renderer, and draws nothing.
            let mut blank = Sceneries::empty();
            blank.register("blank", test::blank);
            let renderer = Renderer::with_sceneries(&device, &queue, format, 8, 8, 1, &blank);
            assert_eq!(renderer.environments()[1], Environment::new("blank"));
            render(&renderer);
        });
    }
}
