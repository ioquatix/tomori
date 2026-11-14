// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The environments a space can be bound to. Each space is bound to exactly one, chosen when it is created; see `design/environments.md`.
use std::fmt;
use std::sync::Arc;

use tomori_environment::ServerSide;
use tomori_protocol::{EnvironmentInfo, EnvironmentVersion};

/// An environment a server offers for spaces: its identifier, which clients map to what they present, the label they show, its protocol version, which a client must present it at, and its layer of the instructions a space's agent is given.
///
/// It may have a server side, which the server runs for each space bound to it; see [`ServerSide`] and `design/environments.md`. Its client side, data sources and tools are planned in [#75](https://github.com/base-rs/tomori/issues/75), [#83](https://github.com/base-rs/tomori/issues/83) and [#82](https://github.com/base-rs/tomori/issues/82). The server offers it only to clients that present it at its version, and a client that joins a space in it without presenting it at that version shows the Minimal grid and names the version, so a headset needs the environment's client side, at that version, to present it.
///
/// ```
/// use tomori_server::Environment;
///
/// let observe = Environment::new("observe", "Observe")
///     .with_instructions("The space shows the service's topology. Ask what a participant is pointing at before investigating it.");
/// assert_eq!(observe.id(), "observe");
/// assert_eq!(observe.label(), "Observe");
/// assert!(observe.instructions().starts_with("The space shows"));
/// ```
#[derive(Clone)]
pub struct Environment {
    id: String,
    label: String,
    version: u32,
    instructions: String,
    side: Option<Arc<dyn ServerSide>>,
    /// Which wall is the space's status wall in it, if it says. See `design/projection.md`.
    status: Option<StatusWall>,
    /// The agent's home in it, its `agent` anchor, if it says, in its own coordinates. See `design/environments.md`.
    agent: Option<tomori_protocol::Pose>,
    /// Its cluster anchors, `home` among them for where its workers stand, each valid and of a name of its own, in its
    /// own coordinates. See `design/environments.md`.
    anchors: Vec<tomori_protocol::cluster::Anchor>,
    /// Where participants arrive in it, its `arrival` anchor, if it says, in its own coordinates.
    arrival: Option<tomori_protocol::Pose>,
    /// Where people arrive as they join a space in it, its spawn points, in its own coordinates.
    spawns: Vec<tomori_protocol::spawn::Spawn>,
    /// The avatar its agent shows as in its spaces, if it says, in place of the one the space's backend names.
    avatar: Option<String>,
    /// The projection walls of its own, in its own coordinates.
    walls: Vec<EnvironmentWall>,
    /// Those walls, carried into a space's coordinates, where home is the origin facing −Z.
    carried: Vec<EnvironmentWall>,
    /// The boxes of its scenery that nothing of Tomori's stands inside nor is seen through, in its own coordinates, each
    /// from its least to its greatest corner. See `design/environments.md`.
    solids: Vec<([f32; 3], [f32; 3])>,
    /// How high above the space's floor the agent hovers as it comes to someone, if it says. See
    /// `design/environments.md`.
    follow_height: Option<f32>,
    /// Its walkable ground, in its own coordinates, if it gives it: the navmesh its clients walk on, which the agent
    /// stands above and keeps clear of. See `design/environments.md`.
    navmesh: Option<Arc<tomori_environment::navmesh::Navmesh>>,
}

/// The lowest and highest an environment may have the agent hover as it comes to someone, in metres above the space's
/// floor: [`Environment::with_follow_height`].
pub const FOLLOW_HEIGHTS: (f32, f32) = (1.0, 10.0);

/// One of an environment's own projection walls: where it stands, in the environment's own coordinates, upright and
/// facing along its pose's +Z, and how it is laid out, its grid of slots, their size and shape, whether it curves and
/// how far it tilts, unless a space changes them: three columns of two rows of standard slots, flat and upright, unless
/// it says otherwise, as a console of wide slots on an arc about where people stand, tilted towards them. It is locked, part of the place, such as
/// a notice board or a console people walk up to, until a space's owner unlocks it. See `design/projection.md`.
///
/// ```
/// use tomori_protocol::{Grid, Pose};
/// use tomori_server::EnvironmentWall;
///
/// let board = Pose::at([3.0, 1.4, -4.0]);
/// let notices = EnvironmentWall::new(board).with_grid(Grid::from((2, 1)));
/// assert_eq!(notices.grid(), Grid::from((2, 1)));
/// assert_eq!(EnvironmentWall::from(board).grid(), Grid::from((3, 2)));
/// let console = EnvironmentWall::new(board)
///     .with_grid(Grid::from((1, 3)))
///     .with_aspect(4.0)
///     .with_curve(5.0)
///     .with_tilt(-15.0);
/// assert_eq!((console.aspect(), console.radius(), console.tilt()), (Some(4.0), Some(5.0), Some(-15.0)));
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnvironmentWall {
    pub pose: tomori_protocol::Pose,
    grid: Option<tomori_protocol::Grid>,
    scale: Option<f32>,
    aspect: Option<f32>,
    radius: Option<f32>,
    tilt: Option<f32>,
    name: Option<&'static str>,
}

/// The most bytes of the name an environment gives its own wall.
pub const WALL_NAME_BYTES: usize = 24;

/// Whether `name` is one an environment may call its own wall, which its label shows in capitals and the agent names it
/// by, as `place: wall Details slot 2`: from 1 to [`WALL_NAME_BYTES`] ASCII letters, digits, spaces and `-`, with no
/// space at either end or two together, that does not begin with `the `, and holds no word `slot`, which would end a
/// wall's name where a slot's number begins, nor begins as a wall is already called, `Tomori`, `status wall`,
/// `environment` or `added`, or as a wall's identity begins, `wall-`, nor whose first word is one a place begins with, `near`,
/// `nearest`, `look`, `looking`, `front`, `beside` or `wall`, in any case, which `set_wall` would read as that form.
///
/// ```
/// use tomori_server::valid_wall_name;
///
/// assert!(valid_wall_name("Details"));
/// assert!(valid_wall_name("Pods under load"));
/// assert!(!valid_wall_name(""));
/// assert!(!valid_wall_name("Tomori wall"));
/// assert!(!valid_wall_name("Slot machine"));
/// ```
pub fn valid_wall_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    (1..=WALL_NAME_BYTES).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b' ' | b'-'))
        && !name.starts_with(' ')
        && !name.ends_with(' ')
        && !name.contains("  ")
        && !lower.starts_with("the ")
        && !lower.split(' ').any(|word| word == "slot")
        && ["tomori", "status wall", "environment", "added", "wall-"]
            .iter()
            .all(|taken| !lower.starts_with(taken))
        && !lower.split(' ').next().is_some_and(|first| {
            [
                "near", "nearest", "look", "looking", "front", "beside", "wall",
            ]
            .contains(&first)
        })
}

impl EnvironmentWall {
    /// A wall at `pose`, of three columns of two rows of standard slots.
    pub const fn new(pose: tomori_protocol::Pose) -> Self {
        Self {
            pose,
            grid: None,
            scale: None,
            aspect: None,
            radius: None,
            tilt: None,
            name: None,
        }
    }

    /// The wall, called `name`, which its label shows in capitals in place of `WALL`, and the space's agent names it by,
    /// as `place: wall Details slot 2`, in place of `environment wall 1`, so that it is asked to put panels on a wall by
    /// what the wall is for. The name must be [`valid_wall_name`]: one that is not is ignored, as is one an earlier wall
    /// of the environment already has, in any case. Without one, the wall is `WALL`, and `environment wall` and its
    /// number among the environment's walls.
    pub fn with_name(mut self, name: &'static str) -> Self {
        self.name = Some(name);
        self
    }

    /// What it is called, if its environment calls it anything the server takes: see [`Self::with_name`].
    pub fn name(&self) -> Option<&'static str> {
        self.name.filter(|name| valid_wall_name(name))
    }

    /// The wall, its slots' width over their height `aspect`, from 1:4 to 4:1, or else a standard slot's shape.
    pub fn with_aspect(mut self, aspect: f32) -> Self {
        self.aspect = Some(aspect);
        self
    }

    /// The wall, curved along an arc about a vertical line `radius` metres ahead of it, at least
    /// `tomori_protocol::wall::LEAST_RADIUS`, or else flat.
    pub fn with_curve(mut self, radius: f32) -> Self {
        self.radius = Some(radius);
        self
    }

    /// The wall, tilted `degrees` about its horizontal middle line, positive with its top towards the people in front
    /// of it, within `tomori_protocol::wall::TILTS`, or else upright.
    pub fn with_tilt(mut self, degrees: f32) -> Self {
        self.tilt = Some(degrees);
        self
    }

    /// Its slots' shape, unless a space changes it: `None` for a standard slot's.
    pub fn aspect(&self) -> Option<f32> {
        self.layout().aspect
    }

    /// The radius it curves along, unless a space changes it: `None` for a flat wall.
    pub fn radius(&self) -> Option<f32> {
        self.layout().radius
    }

    /// How far it tilts, in degrees, unless a space changes it: `None` upright.
    pub fn tilt(&self) -> Option<f32> {
        self.layout().tilt
    }

    /// The wall, with `grid` of slots, within a wall's bounds, or else three columns of two rows.
    pub fn with_grid(mut self, grid: tomori_protocol::Grid) -> Self {
        self.grid = Some(grid);
        self
    }

    /// The wall, its slots `scale` times a standard slot's, from half to five times, or else a standard slot's.
    pub fn with_scale(mut self, scale: f32) -> Self {
        self.scale = Some(scale);
        self
    }

    /// Its grid of slots, as it is unless a space changes it.
    pub fn grid(&self) -> tomori_protocol::Grid {
        self.layout().grid
    }

    /// How it is laid out unless a space changes it: see [`Self::checked`].
    pub(crate) fn layout(&self) -> crate::stage::Layout {
        self.checked().0
    }

    /// How it is laid out unless a space changes it, and why not as it says, if it is not: each of its grid, scale,
    /// aspect, curve and tilt within its bounds, or else, unsaid, a flat wall's; and then the whole as a participant's
    /// change must be, by the stage's own rule: curved no further round than half a circle, and at most 16 m wide and
    /// 9 m tall. A layout that is not, as eight wide columns at twice a standard slot, or three wide columns curved at
    /// 1 m, is as though its shape, curve and tilt were unsaid, and, should its grid and scale still not be, as a flat
    /// wall's, so that a declared wall is never one the space could not then change.
    pub(crate) fn checked(&self) -> (crate::stage::Layout, Option<&'static str>) {
        let flat = crate::stage::Layout::FLAT;
        let scale = self
            .scale
            .filter(|scale| {
                scale.is_finite()
                    && (tomori_protocol::wall::SCALES.0..=tomori_protocol::wall::SCALES.1)
                        .contains(scale)
            })
            .unwrap_or(flat.scale);
        let within = |value: Option<f32>, (least, most): (f32, f32)| {
            value.filter(|value| value.is_finite() && (least..=most).contains(value))
        };
        let said = crate::stage::Layout {
            grid: self.grid.filter(|grid| grid.valid()).unwrap_or(flat.grid),
            scale,
            aspect: within(self.aspect, tomori_protocol::wall::ASPECTS),
            radius: within(
                self.radius,
                (
                    tomori_protocol::wall::LEAST_RADIUS,
                    crate::stage::LONGEST_RADIUS,
                ),
            ),
            tilt: within(self.tilt, tomori_protocol::wall::TILTS).filter(|tilt| *tilt != 0.0),
            content_scale: None,
        };
        let fits = |layout: crate::stage::Layout| {
            let wall = layout.apply(tomori_protocol::Wall::new(
                "declared",
                tomori_protocol::WallKind::Environment,
                self.pose,
            ));
            crate::stage::Stage::fits(&wall, layout)
        };
        let Err(why) = fits(said) else {
            return (said, None);
        };
        let plain = crate::stage::Layout {
            grid: said.grid,
            scale: said.scale,
            ..flat
        };
        match fits(plain) {
            Ok(()) => (plain, Some(why)),
            Err(_) => (flat, Some(why)),
        }
    }
}

impl From<tomori_protocol::Pose> for EnvironmentWall {
    fn from(pose: tomori_protocol::Pose) -> Self {
        Self::new(pose)
    }
}

impl PartialEq<tomori_protocol::Pose> for EnvironmentWall {
    fn eq(&self, pose: &tomori_protocol::Pose) -> bool {
        self.pose == *pose
    }
}

/// Which wall is a space's status wall in an environment, where Tomori's notices, what the agent projects unasked and
/// every placement's last fallback go: one of the environment's own walls, as a farm's notice board, or a wall of the
/// space's own that it stands and lays out. Without one, the space's own status wall stands beneath the agent's home.
/// It never travels with the agent. See `design/projection.md`.
///
/// ```
/// use tomori_protocol::{Grid, Pose};
/// use tomori_server::{EnvironmentWall, StatusWall};
///
/// assert_eq!(StatusWall::own(0).index(), Some(0));
/// let strip = EnvironmentWall::new(Pose::at([0.0, 1.2, -2.5])).with_grid(Grid::from((3, 1)));
/// assert_eq!(StatusWall::at(strip).wall(), Some(strip));
/// assert_eq!(StatusWall::own(0).wall(), None);
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StatusWall(Role);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Role {
    Own(usize),
    At(EnvironmentWall),
}

impl StatusWall {
    /// The environment's own wall `index`, by its place among those it declares with [`Environment::with_walls`],
    /// from 0, as a side's `Panel::on_wall` names one: that wall is the status wall, its pose, grid, lock and scenery
    /// the environment's, and the space's own status wall is not shown there.
    pub const fn own(index: usize) -> Self {
        Self(Role::Own(index))
    }

    /// The space's own status wall, standing and laid out as `wall` says, in the environment's own coordinates, carried
    /// into the space's as every anchor is: the middle of it there, facing along its +Z, its grid, slots, curve and
    /// tilt the default the space's edits stand over.
    pub fn at(wall: impl Into<EnvironmentWall>) -> Self {
        Self(Role::At(wall.into()))
    }

    /// The environment's own wall it names, by its place among them, if it names one.
    pub fn index(&self) -> Option<usize> {
        match self.0 {
            Role::Own(index) => Some(index),
            Role::At(_) => None,
        }
    }

    /// The wall it stands and lays out, if it gives one.
    pub fn wall(&self) -> Option<EnvironmentWall> {
        match self.0 {
            Role::At(wall) => Some(wall),
            Role::Own(_) => None,
        }
    }
}

/// The fewest slots a status wall has, so that two notices can stand together.
pub const STATUS_SLOTS: u32 = 2;

/// `pose`, in a space's coordinates, in those of the environment whose upright `carriage` carries them there: the
/// carriage undone, its turn about the vertical taken off the pose's own, whatever its pitch and roll.
pub(crate) fn uncarried(
    carriage: &tomori_protocol::Pose,
    pose: &tomori_protocol::Pose,
) -> tomori_protocol::Pose {
    let yaw = carriage.yaw();
    let (sin, cos) = yaw.sin_cos();
    let [dx, dy, dz] = [
        pose.position[0] - carriage.position[0],
        pose.position[1] - carriage.position[1],
        pose.position[2] - carriage.position[2],
    ];
    let (s, c) = (-yaw / 2.0).sin_cos();
    let [x, y, z, w] = pose.orientation;
    tomori_protocol::Pose {
        position: [dx * cos - dz * sin, dy, dx * sin + dz * cos],
        orientation: [c * x + s * z, c * y + s * w, c * z - s * x, c * w - s * y],
    }
}

/// `anchor`, its label cleaned onto one line and cut after `tomori_environment::ANCHOR_LABEL_LIMIT` characters, as an
/// object's description is, and not said to be placed at run time, whatever it says.
pub(crate) fn cleaned(
    mut anchor: tomori_protocol::cluster::Anchor,
) -> tomori_protocol::cluster::Anchor {
    anchor.label = anchor
        .label
        .map(|label| {
            crate::space::one_line(&label, tomori_environment::ANCHOR_LABEL_LIMIT)
                .trim()
                .to_owned()
        })
        .filter(|label| !label.is_empty());
    anchor.placed = false;
    anchor
}

impl fmt::Debug for Environment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Environment")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("version", &self.version)
            .field("instructions", &self.instructions)
            .field("server_side", &self.side.is_some())
            .field("status", &self.status)
            .field("agent", &self.agent)
            .field("anchors", &self.anchors)
            .field("arrival", &self.arrival)
            .field("spawns", &self.spawns)
            .field("avatar", &self.avatar)
            .field("walls", &self.walls)
            .field("solids", &self.solids)
            .field("follow_height", &self.follow_height)
            .field("navmesh", &self.navmesh)
            .finish()
    }
}

/// Equal when they offer the same identifier, label, version, instructions, anchors, walls, solids and ground, and the same server side, which is the same one rather than one like it.
impl PartialEq for Environment {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.label == other.label
            && self.version == other.version
            && self.instructions == other.instructions
            && self.status == other.status
            && self.agent == other.agent
            && self.anchors == other.anchors
            && self.arrival == other.arrival
            && self.spawns == other.spawns
            && self.avatar == other.avatar
            && self.walls == other.walls
            && self.solids == other.solids
            && self.follow_height == other.follow_height
            && self.navmesh == other.navmesh
            && match (&self.side, &other.side) {
                (Some(side), Some(other)) => Arc::ptr_eq(side, other),
                (None, None) => true,
                _ => false,
            }
    }
}

impl Eq for Environment {}

impl Environment {
    /// The environment `id`, which clients show as `label`, at protocol version 1, contributing no instructions.
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            version: 1,
            instructions: String::new(),
            side: None,
            status: None,
            agent: None,
            anchors: Vec::new(),
            arrival: None,
            spawns: Vec::new(),
            avatar: None,
            walls: Vec::new(),
            carried: Vec::new(),
            solids: Vec::new(),
            follow_height: None,
            navmesh: None,
        }
    }

    /// The environment, running `side` for each space bound to it, from when the space is bound until it is bound to another environment or closes, or the server stops. See `design/environments.md`.
    ///
    /// ```
    /// use tomori_environment::{Result, ServerSide, Space};
    /// use tomori_server::Environment;
    ///
    /// struct Watch;
    ///
    /// impl ServerSide for Watch {
    ///     fn run(&self, space: &Space) -> Result<()> {
    ///         while !space.cancellation().is_cancelled() {
    ///             std::thread::sleep(std::time::Duration::from_millis(10));
    ///         }
    ///         Ok(())
    ///     }
    /// }
    ///
    /// let observe = Environment::new("observe", "Observe").with_server_side(Watch);
    /// assert!(observe.server_side().is_some());
    /// ```
    pub fn with_server_side(mut self, side: impl ServerSide + 'static) -> Self {
        self.side = Some(Arc::new(side));
        self
    }

    /// Its server side, if it has one.
    pub fn server_side(&self) -> Option<&Arc<dyn ServerSide>> {
        self.side.as_ref()
    }

    /// The environment, telling a space's agent `instructions` about what it presents and how to use it, as the `tomori-environment` layer of the space's instructions; see `design/spaces.md`. Empty, it contributes none.
    pub fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = instructions.into();
        self
    }

    /// The environment, at protocol `version`, which a client must present it at to be offered it. Raise it when a
    /// client side of an earlier version could not present the environment; a change it can ignore keeps it. See
    /// `design/environments.md`.
    ///
    /// ```
    /// use tomori_server::Environment;
    ///
    /// assert_eq!(Environment::new("observe", "Observe").version(), 1);
    /// assert_eq!(Environment::new("observe", "Observe").with_version(2).version(), 2);
    /// ```
    pub fn with_version(mut self, version: u32) -> Self {
        self.version = version;
        self
    }

    /// Its protocol version, 1 unless it was given another.
    pub fn version(&self) -> u32 {
        self.version
    }

    /// The identifier clients name it by, in a space's state and in creating or changing a space.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The label clients show for it.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Its layer of a space's instructions, empty when it contributes none.
    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    /// The environment, with participants arriving at `arrival`, in its own coordinates, facing along the pose's +Z, as
    /// every anchor faces (see [`tomori_protocol::Pose::facing_towards`]): its home. A space's coordinates always have
    /// home at their origin, facing −Z, so the server and every client that draws the environment carry its own
    /// coordinates into the space's: its scenery, walkable surfaces and client side, and every anchor and wall it gives,
    /// which are given in its own coordinates too. Without one, participants arrive at its origin, facing −Z, and
    /// nothing moves. The server uses it if it is finite, within 1 km of its origin and has an orientation, made
    /// upright. Travel from there is free. See `design/environments.md`.
    ///
    /// ```
    /// use tomori_protocol::Pose;
    /// use tomori_server::Environment;
    ///
    /// // A street, looking up at a balcony.
    /// let street = Pose::at([0.0, 0.0, 8.0]).facing_towards([0.0, 4.0, 0.0]);
    /// assert_eq!(Environment::new("town", "Town").with_arrival(street).arrival(), Some(street));
    /// assert_eq!(Environment::new("town", "Town").arrival(), None);
    /// ```
    pub fn with_arrival(mut self, arrival: tomori_protocol::Pose) -> Self {
        self.arrival = Some(arrival);
        self.carry_walls();
        self
    }

    /// Where participants arrive in it, if it says, in its own coordinates.
    pub fn arrival(&self) -> Option<tomori_protocol::Pose> {
        self.arrival
    }

    /// The environment, with people joining a space in it arriving at `spawn` too, a spawn point in its own
    /// coordinates, where the middle of their room arrives on its floor, facing along the pose's +Z, as every anchor
    /// faces, rather than all at its arrival, on top of one another. The server chooses one for each person as they
    /// join or rejoin the space, or it changes environment, and never moves them later: of the points tagged for their
    /// role, `tomori_protocol::spawn::OWNER`, `COLLABORATOR` or `VISITOR`, or, with none, those untagged, the one
    /// fewest stand at, and of those the one fewest have arrived at; the spectator watches from one tagged `SPECTATOR`,
    /// or else untagged, while it follows nobody. Someone no point suits arrives at its arrival, as everyone does in an
    /// environment that declares none. One whose tags `tomori_protocol::spawn::Spawn::checked` refuses, whose pose is
    /// not finite or has no orientation, or beyond `tomori_protocol::spawn::SPAWN_LIMIT`, is left out with a warning;
    /// one beyond 30 m of its arrival on an axis, once carried, is left out of the space. See
    /// `design/environments.md`.
    ///
    /// ```
    /// use tomori_protocol::Pose;
    /// use tomori_protocol::spawn::{self, Spawn};
    /// use tomori_server::Environment;
    ///
    /// let gate = Spawn::new(Pose::at([0.0, 0.0, 6.0]).facing_towards([0.0, 0.0, 0.0])).with_tag(spawn::VISITOR);
    /// let farm = Environment::new("farm", "Farm").with_spawn(gate.clone());
    /// assert_eq!(farm.spawns(), [gate]);
    /// assert!(Environment::new("farm", "Farm").spawns().is_empty());
    /// ```
    pub fn with_spawn(mut self, spawn: tomori_protocol::spawn::Spawn) -> Self {
        let refused = spawn
            .checked()
            .err()
            .or_else(|| {
                crate::stage::valid_arrival(&spawn.pose)
                    .is_none()
                    .then_some("its pose is not finite, or has no orientation")
            })
            .or_else(|| {
                (self.spawns.len() >= tomori_protocol::spawn::SPAWN_LIMIT)
                    .then_some("it declares 32 spawn points before it")
            });
        match refused {
            Some(why) => log::warn!(
                "Environment {}'s spawn point {:?} is left out: {why}.",
                self.id,
                spawn.pose.position
            ),
            None => self.spawns.push(spawn),
        }
        self
    }

    /// Where people arrive as they join a space in it, its spawn points, in its own coordinates.
    pub fn spawns(&self) -> &[tomori_protocol::spawn::Spawn] {
        &self.spawns
    }

    /// The carriage from its own coordinates into a space's: [`tomori_protocol::Pose::carriage`] of its arrival, if it
    /// gives a valid one, or none, which leaves everything where it is.
    pub(crate) fn carriage(&self) -> Option<tomori_protocol::Pose> {
        self.arrival
            .as_ref()
            .and_then(crate::stage::valid_arrival)
            .map(|arrival| tomori_protocol::Pose::carriage(&arrival))
    }

    /// `pose`, in its own coordinates, in a space's: carried, made upright and bounded as walls are, or none.
    fn carried(&self, pose: Option<&tomori_protocol::Pose>) -> Option<tomori_protocol::Pose> {
        match self.carriage() {
            None => crate::stage::valid(pose?),
            Some(carriage) => {
                crate::stage::valid(&carriage.carry(&crate::stage::valid_arrival(pose?)?))
            }
        }
    }

    fn carry_walls(&mut self) {
        let carriage = self.carriage();
        self.carried =
            self.walls
                .iter()
                .filter_map(|wall| {
                    let pose = match carriage {
                        None => Some(wall.pose),
                        Some(carriage) => crate::stage::valid_arrival(&wall.pose)
                            .map(|pose| carriage.carry(&pose)),
                    }?;
                    Some(EnvironmentWall { pose, ..*wall })
                })
                .collect();
    }

    /// The environment, with the space's status wall where `status` says: one of its own walls, as a notice board the
    /// place already has, or a wall of the space's own it stands and lays out, such as a compact strip by a door. Without
    /// one, the space's status wall stands beneath the agent's home (see [`Self::tomori_wall_home`]). One of its own
    /// walls must be one it declares, and either must have at least [`STATUS_SLOTS`] slots: one that is not is left out,
    /// with a warning, and the default stands. Make the wall readable: one it stands from where participants arrive,
    /// and one of its own from the ground in front of it, and seen from where they arrive (see
    /// `tomori_environment::sightlines`). What is on the status wall belongs to the space and moves with it. See
    /// `design/projection.md`.
    ///
    /// ```
    /// use tomori_protocol::{Grid, Pose};
    /// use tomori_server::{Environment, EnvironmentWall, StatusWall};
    ///
    /// let board = EnvironmentWall::new(Pose::at([3.0, 1.4, -4.0])).with_grid(Grid::from((2, 1)));
    /// let farm = Environment::new("farm", "Farm").with_walls([board]).with_status_wall(StatusWall::own(0));
    /// assert_eq!(farm.status_wall(), Some(StatusWall::own(0)));
    /// assert_eq!(Environment::new("plain", "Plain").status_wall(), None);
    /// ```
    pub fn with_status_wall(mut self, status: StatusWall) -> Self {
        self.status = Some(status);
        self.check_status();
        self
    }

    /// Which wall is the space's status wall in it, if it says.
    pub fn status_wall(&self) -> Option<StatusWall> {
        self.status
    }

    /// The environment, with the space's status wall standing at `stage`, its `stage` anchor, in its own coordinates,
    /// upright and facing along the pose's +Z, laid out as the default status wall is, six slots in a curved row: as
    /// [`Self::with_status_wall`] with [`StatusWall::at`], which also lays it out, and which it is kept for.
    ///
    /// ```
    /// use tomori_protocol::Pose;
    /// use tomori_server::Environment;
    ///
    /// let stage = Pose::at([0.0, 1.2, -2.5]).facing_towards([0.0, 1.6, 0.0]);
    /// assert_eq!(Environment::new("observe", "Observe").with_stage(stage).stage(), Some(stage));
    /// assert_eq!(Environment::new("observe", "Observe").stage(), None);
    /// ```
    pub fn with_stage(self, stage: tomori_protocol::Pose) -> Self {
        let layout = crate::stage::Layout::TOMORI;
        let wall = EnvironmentWall::new(stage)
            .with_grid(layout.grid)
            .with_scale(layout.scale)
            .with_curve(crate::stage::ARC_RADIUS);
        self.with_status_wall(StatusWall::at(wall))
    }

    /// Where the space's status wall stands in it, if it stands one there, in its own coordinates.
    pub fn stage(&self) -> Option<tomori_protocol::Pose> {
        self.status
            .and_then(|status| status.wall())
            .map(|wall| wall.pose)
    }

    /// Warn of a status wall it names that is left out: one of its own walls it does not declare, or a wall of fewer
    /// than [`STATUS_SLOTS`].
    fn check_status(&self) {
        if let Some(status) = self.status
            && let Err(why) = self.checked_status(status)
        {
            log::warn!(
                "Environment {}'s status wall is left out, and the default stands: {why}",
                self.id
            );
        }
    }

    /// `status`, if the server uses it here, or why not.
    fn checked_status(&self, status: StatusWall) -> Result<(), &'static str> {
        let layout = match status.0 {
            Role::Own(index) => self
                .walls
                .get(index)
                .filter(|_| index < crate::stage::DECLARED_WALLS)
                .ok_or("it names a wall the environment does not declare")?
                .layout(),
            Role::At(wall) => wall.layout(),
        };
        if layout.grid.slots() < STATUS_SLOTS {
            return Err("it has fewer than two slots");
        }
        Ok(())
    }

    /// Which wall is the space's status wall in a space in it: one of its own, by its place among them; or the space's
    /// own, at [`Self::tomori_wall_home`] and laid out as it says, or by default; or, beyond reach, by default.
    pub(crate) fn status(&self) -> crate::stage::Status {
        match self
            .status
            .filter(|status| self.checked_status(*status).is_ok())
        {
            Some(StatusWall(Role::Own(index))) => crate::stage::Status::Own(index),
            Some(StatusWall(Role::At(wall))) if self.carried(Some(&wall.pose)).is_some() => {
                crate::stage::Status::At(wall.layout())
            }
            _ => crate::stage::Status::Beneath,
        }
    }

    /// Where the space's own status wall's home stands in a space in it, in the space's coordinates: the wall its
    /// [`StatusWall::at`] stands, carried, if it gives a valid one, or else beneath its agent's home carried, if it gives
    /// a valid one, as `tomori_protocol::stance::wall_beneath` stands it on the floor where participants arrive; or
    /// none, for the default, [`crate::stage::default_stage`], beneath the agent's default home. Where its status wall is
    /// one of its own, the space's own is not shown, and this is where it would stand. It never travels with the agent.
    ///
    /// ```
    /// use tomori_protocol::{Pose, cluster::FLOOR, stance};
    /// use tomori_server::Environment;
    ///
    /// let pod = Pose::at([2.0, 1.6, -4.0]).facing_towards([0.0, 1.6, 0.0]);
    /// let observe = Environment::new("observe", "Observe").with_agent(pod);
    /// assert_eq!(observe.tomori_wall_home(), Some(stance::wall_beneath(&pod, FLOOR)));
    /// let stage = Pose::at([0.0, 1.2, -2.5]).facing_towards([0.0, 1.6, 0.0]);
    /// assert_eq!(observe.with_stage(stage).tomori_wall_home(), Some(stage));
    /// assert_eq!(Environment::new("plain", "Plain").tomori_wall_home(), None);
    /// ```
    pub fn tomori_wall_home(&self) -> Option<tomori_protocol::Pose> {
        self.carried(self.stage().as_ref()).or_else(|| {
            let agent = self.carried(self.agent.as_ref())?;
            crate::stage::valid(&tomori_protocol::stance::wall_beneath(
                &agent,
                tomori_protocol::cluster::FLOOR,
            ))
        })
    }

    /// The environment, with projection walls of its own, `walls`, in its own coordinates, each a pose, upright and
    /// facing along its +Z, or an [`EnvironmentWall`], which says its grid and the size of its slots too, of which the
    /// server uses the first sixty-four, [`tomori_protocol::DECLARED_WALLS`], ignoring any more. They are part of the place, locked until a space's owner
    /// unlocks them, such as a notice board or a console people walk up to and put what the agent shows on. The agent
    /// projects onto them, as onto the Tomori wall, for whoever asks nearest them, and what is on them is kept for the
    /// space in this environment, hidden while the space is in another. See `design/projection.md`.
    ///
    /// ```
    /// use tomori_protocol::{Grid, Pose};
    /// use tomori_server::{Environment, EnvironmentWall};
    ///
    /// let board = Pose { position: [3.0, 1.4, -4.0], orientation: [0.0, 0.0, 0.0, 1.0] };
    /// assert_eq!(Environment::new("observe", "Observe").with_walls([board]).walls(), [board]);
    /// let notices = EnvironmentWall::new(board).with_grid(Grid::from((2, 1)));
    /// assert_eq!(Environment::new("observe", "Observe").with_walls([notices]).walls(), [notices]);
    /// ```
    pub fn with_walls(
        mut self,
        walls: impl IntoIterator<Item = impl Into<EnvironmentWall>>,
    ) -> Self {
        let mut walls: Vec<EnvironmentWall> = walls.into_iter().map(Into::into).collect();
        // A name two walls would share names the first alone, so that the agent's `place: wall NAME` is never ambiguous.
        for index in 1..walls.len() {
            let taken = walls[..index].iter().any(|other| {
                other
                    .name()
                    .zip(walls[index].name())
                    .is_some_and(|(taken, name)| taken.eq_ignore_ascii_case(name))
            });
            if taken {
                walls[index].name = None;
            }
        }
        for (index, wall) in walls.iter().enumerate() {
            if let (_, Some(why)) = wall.checked() {
                log::warn!(
                    "Environment {}'s wall {index} is laid out as a flat wall's, not as it says: {why}",
                    self.id
                );
            }
        }
        self.walls = walls;
        self.carry_walls();
        self.check_status();
        self
    }

    /// Its own projection walls, in its own coordinates.
    pub fn walls(&self) -> &[EnvironmentWall] {
        &self.walls
    }

    /// The environment, with the agent's home at `agent`, its `agent` anchor, in its own coordinates: the swarm's middle
    /// there, its eye facing along the pose's +Z, for everyone in a space in it, such as a balcony looking down at
    /// where participants arrive, `Pose::at(balcony).facing_towards(arrival)`. The agent appears there as the space
    /// opens, and returns there when sent home or once a conversation it followed is quiet. Without one, the swarm
    /// stands 1.4 m up and 2.3 m straight ahead of home, facing home. The status wall's home is beneath it, unless
    /// [`Self::with_status_wall`] says otherwise; only then keep it at least `tomori_protocol::stance::LOWEST_AGENT`,
    /// 1.4 m, above the floor, for the wall to stand beneath it with a gap. Make it seen from where participants
    /// arrive, and a wall beneath it read: see `tomori_environment::sightlines`. The server sends it in the space's
    /// state, carried into the space's coordinates, if it is then finite, within 30 m of home and has an orientation,
    /// made upright, and leaves it out otherwise. See `design/environments.md`.
    ///
    /// ```
    /// use tomori_protocol::Pose;
    /// use tomori_server::Environment;
    ///
    /// let pod = Pose::at([2.0, 1.4, -4.0]).facing_towards([0.0, 1.6, 0.0]);
    /// assert_eq!(Environment::new("observe", "Observe").with_agent(pod).agent(), Some(pod));
    /// assert_eq!(Environment::new("observe", "Observe").agent(), None);
    /// ```
    pub fn with_agent(mut self, agent: tomori_protocol::Pose) -> Self {
        self.agent = Some(agent);
        self
    }

    /// Where the agent's home is in it, if it says, in its own coordinates.
    pub fn agent(&self) -> Option<tomori_protocol::Pose> {
        self.agent
    }

    /// The environment, with the middle of its workers' arrangement at `workers`, its `workers` anchor, in its own
    /// coordinates, the arrangement facing along the pose's +Z, towards whoever watches it, as the agent does: the
    /// compatible form of [`Self::with_anchor`] for its `home` anchor, taking the room today's arc takes, in place of
    /// any `home` anchor given before. Without one, the arrangement's middle is the agent's home, so that workers stand
    /// about the agent's home, as they stand about its default home by default; they stay there wherever the agent goes.
    /// A client places them about it, by the placement its build registers for the environment or the default arc.
    /// Sent and bounded as [`Self::with_agent`] is. See `design/environments.md`.
    ///
    /// ```
    /// use tomori_protocol::Pose;
    /// use tomori_server::Environment;
    ///
    /// let region = Pose::at([0.0, 1.4, -6.0]).facing_towards([0.0, 1.6, 0.0]);
    /// assert_eq!(Environment::new("observe", "Observe").with_workers(region).workers(), Some(region));
    /// assert_eq!(Environment::new("observe", "Observe").workers(), None);
    /// ```
    pub fn with_workers(mut self, workers: tomori_protocol::Pose) -> Self {
        self.anchors.retain(|anchor| !anchor.is_home());
        self.anchors.push(tomori_protocol::cluster::Anchor::new(
            tomori_protocol::cluster::HOME,
            workers,
        ));
        self
    }

    /// Where the middle of its workers' arrangement is, its `home` anchor's pose, if it says, in its own coordinates.
    pub fn workers(&self) -> Option<tomori_protocol::Pose> {
        self.home().map(|home| home.pose)
    }

    /// The environment, with the cluster anchor `anchor`, in its own coordinates, its point facing along its +Z towards
    /// whoever watches it: a named place that workers, panels and the agent gather at, laid out by
    /// `tomori_protocol::cluster`. The anchor named `home` is where its workers stand by default: about the agent's
    /// home, gathered about the eyes at the arrival, with the agent at its point; given an extent, fitted to it, today's
    /// arc while it fits, then an arc, a grid, compact and a "+N more" sign; without one, on today's arc, as
    /// [`Self::with_workers`] declares it; or, with `tomori_protocol::cluster::Formation::Ring`, evenly about the agent's
    /// home on the ring drawn there, in rows, then compact, as the farmyard's does, where the arc is the default, as in
    /// Sky. Others, such as `aisle-3`, are places the agent may put workers, panels and
    /// itself, which it finds by their kind and label in `presence`, each laid out the same way about its audience,
    /// 2.3 m before it. Check their room with `tomori_environment::sightlines::fits`. One whose name, extent, kind or
    /// object `tomori_protocol::cluster::Anchor::checked` refuses, whose name another before it has, or beyond
    /// `tomori_environment::ANCHOR_LIMIT`, is left out with a warning; its label is cleaned onto one line and cut after
    /// `tomori_environment::ANCHOR_LABEL_LIMIT` characters; its pose is sent and bounded as [`Self::with_agent`]'s is.
    /// See `design/environments.md`.
    ///
    /// ```
    /// use tomori_protocol::Pose;
    /// use tomori_protocol::cluster::Anchor;
    /// use tomori_server::Environment;
    ///
    /// let yard = Pose::at([0.0, 1.4, -5.0]).facing_towards([0.0, 1.6, 0.0]);
    /// let home = Anchor::new("home", yard).with_extent(6.0, 2.5, 3.0);
    /// let farm = Environment::new("farm", "Farm").with_anchor(home.clone());
    /// assert_eq!(farm.anchors(), [home]);
    /// assert_eq!(farm.workers(), Some(yard));
    /// ```
    pub fn with_anchor(mut self, anchor: tomori_protocol::cluster::Anchor) -> Self {
        let refused = anchor
            .checked()
            .err()
            .or_else(|| {
                self.anchors
                    .iter()
                    .any(|kept| kept.name == anchor.name)
                    .then_some("another of its name comes before it")
            })
            .or_else(|| {
                (self.anchors.len() >= tomori_environment::ANCHOR_LIMIT)
                    .then_some("it declares 64 anchors before it")
            });
        match refused {
            Some(why) => log::warn!(
                "Environment {}'s anchor {:?} is left out: {why}.",
                self.id,
                anchor.name
            ),
            None => self.anchors.push(cleaned(anchor)),
        }
        self
    }

    /// Its cluster anchors, in its own coordinates.
    pub fn anchors(&self) -> &[tomori_protocol::cluster::Anchor] {
        &self.anchors
    }

    /// Its `home` anchor, where its workers stand, if it gives one, in its own coordinates.
    pub fn home(&self) -> Option<&tomori_protocol::cluster::Anchor> {
        self.anchors.iter().find(|anchor| anchor.is_home())
    }

    /// The environment, with the agent showing as the avatar `avatar` in its spaces, by identifier, in place of the one
    /// the space's backend names, such as a character that suits the scene. The backend's avatar is the default; an
    /// environment overrides it. The server sends it in the space's state if `tomori_protocol::valid_avatar` takes it,
    /// and the backend's otherwise; a client whose build lacks it shows the swarm. See `design/avatars.md`.
    ///
    /// ```
    /// use tomori_server::Environment;
    ///
    /// assert_eq!(Environment::new("farm", "Farm").with_avatar("hen").avatar(), Some("hen"));
    /// assert_eq!(Environment::new("farm", "Farm").avatar(), None);
    /// ```
    pub fn with_avatar(mut self, avatar: impl Into<String>) -> Self {
        self.avatar = Some(avatar.into());
        self
    }

    /// The avatar its agent shows as in its spaces, if it says.
    pub fn avatar(&self) -> Option<&str> {
        self.avatar.as_deref()
    }

    /// The environment, with `solids`, boxes of its scenery, in its own coordinates, each from its least to its greatest
    /// corner, aligned with its axes, such as a building or a stack of hay: nothing of Tomori's that moves about the
    /// space, the agent where it goes and the panels floating beside someone, stands inside one, nor is seen through
    /// one. The same boxes its tests check its homes against with `tomori_environment::sightlines`. See
    /// `design/environments.md`.
    ///
    /// ```
    /// use tomori_server::Environment;
    ///
    /// let barn = ([2.0, 0.0, -6.0], [5.0, 3.0, -3.0]);
    /// assert_eq!(Environment::new("farm", "Farm").with_solids([barn]).solids(), [barn]);
    /// ```
    pub fn with_solids(mut self, solids: impl IntoIterator<Item = ([f32; 3], [f32; 3])>) -> Self {
        self.solids = solids.into_iter().collect();
        self
    }

    /// The boxes of its scenery, in its own coordinates.
    pub fn solids(&self) -> &[([f32; 3], [f32; 3])] {
        &self.solids
    }

    /// The environment, with `navmesh`, its walkable ground, in its own coordinates: the same ground its clients'
    /// teleport profile lands participants on, derived by `tomori_environment::navmesh::Navmesh::derive` from what it
    /// draws as ground, less what stands on it, from the description its crate shares between its server and its
    /// clients. The agent following someone stands on their level, as high above the floor beneath it as their eyes
    /// stand above theirs, and goes there above the ground, never through a floor nor up from beneath one. See
    /// `design/environments.md`.
    ///
    /// ```
    /// use glam::Vec3;
    /// use tomori_environment::navmesh::Navmesh;
    /// use tomori_server::Environment;
    ///
    /// let deck = [Vec3::new(-2.0, 3.0, -2.0), Vec3::new(2.0, 3.0, -2.0), Vec3::new(0.0, 3.0, 2.0)];
    /// let ground = Navmesh::derive([deck], &[]);
    /// let farm = Environment::new("farm", "Farm").with_navmesh(ground.clone());
    /// assert_eq!(farm.navmesh(), Some(&ground));
    /// assert_eq!(Environment::new("farm", "Farm").navmesh(), None);
    /// ```
    pub fn with_navmesh(mut self, navmesh: tomori_environment::navmesh::Navmesh) -> Self {
        self.navmesh = Some(Arc::new(navmesh));
        self
    }

    /// Its walkable ground, in its own coordinates, if it gives it.
    pub fn navmesh(&self) -> Option<&tomori_environment::navmesh::Navmesh> {
        self.navmesh.as_deref()
    }

    /// The environment, whose agent hovers `metres` above the space's floor, the floor where participants arrive, as
    /// it comes to someone, following the speaker, called or asked to go to them or beside something, rather than a
    /// little above their eyes, such as one whose home floats high over the scene; but never lower than it would stand
    /// before someone's eyes higher than that, as on a raised platform, so that it stands above them or level with
    /// them, as far from them across the floor as ever, and never in front of their face below their eyes. One not from
    /// [`FOLLOW_HEIGHTS`]`.0` to `.1`, 1 to 10 m, is left out with a warning, keeping the default. See
    /// `design/environments.md`.
    ///
    /// ```
    /// use tomori_server::Environment;
    ///
    /// assert_eq!(Environment::new("observe", "Observe").with_follow_height(5.1).follow_height(), Some(5.1));
    /// assert_eq!(Environment::new("observe", "Observe").with_follow_height(12.0).follow_height(), None);
    /// assert_eq!(Environment::new("observe", "Observe").follow_height(), None);
    /// ```
    pub fn with_follow_height(mut self, metres: f32) -> Self {
        if (FOLLOW_HEIGHTS.0..=FOLLOW_HEIGHTS.1).contains(&metres) {
            self.follow_height = Some(metres);
        } else {
            log::warn!(
                "Environment {}'s follow height of {metres} m is left out, as it is not from 1 to 10 m, and the agent stands a little above the eyes of whoever it comes to.",
                self.id
            );
        }
        self
    }

    /// How high above the space's floor its agent hovers as it comes to someone, if it says.
    pub fn follow_height(&self) -> Option<f32> {
        self.follow_height
    }

    /// Its solids, carried into a space's coordinates, each the box about its corners carried there, those not finite
    /// or beyond [`crate::stage::ARRIVAL_REACH`] left out.
    fn carried_solids(&self) -> Vec<tomori_environment::sightlines::Solid> {
        let carriage = self.carriage();
        self.solids
            .iter()
            .filter(|(low, high)| {
                low.iter()
                    .chain(high)
                    .all(|value| value.is_finite() && value.abs() <= crate::stage::ARRIVAL_REACH)
            })
            .map(|(low, high)| {
                let corners = (0..8).map(|corner: usize| {
                    let point = std::array::from_fn(|axis| {
                        if corner & (1 << axis) == 0 {
                            low[axis].min(high[axis])
                        } else {
                            low[axis].max(high[axis])
                        }
                    });
                    match carriage {
                        None => point,
                        Some(carriage) => {
                            carriage.carry(&tomori_protocol::Pose::at(point)).position
                        }
                    }
                });
                corners.fold(
                    tomori_environment::sightlines::Solid::new([f32::MAX; 3], [f32::MIN; 3]),
                    |solid, point| {
                        tomori_environment::sightlines::Solid::new(
                            std::array::from_fn(|axis| solid.low[axis].min(point[axis])),
                            std::array::from_fn(|axis| solid.high[axis].max(point[axis])),
                        )
                    },
                )
            })
            .collect()
    }
}

/// The environments a server offers, in order: the first is the one a space whose creator names none is bound to. None by default: an application registers those it offers, each by the identity in its own crate. See `design/environments.md`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Environments(Vec<Environment>);

impl Environments {
    /// The environments offered, in order.
    pub fn new(environments: impl IntoIterator<Item = Environment>) -> Self {
        Self(environments.into_iter().collect())
    }

    /// Offer `environment` too, in place of one with the same identifier, or else after the others.
    pub fn with(mut self, environment: Environment) -> Self {
        match self
            .0
            .iter_mut()
            .find(|offered| offered.id == environment.id)
        {
            Some(offered) => *offered = environment,
            None => self.0.push(environment),
        }
        self
    }

    /// The environment offered with the identifier `id`, if there is one.
    pub fn get(&self, id: &str) -> Option<&Environment> {
        self.0.iter().find(|environment| environment.id == id)
    }

    /// The environments offered, in order, as a slice.
    pub fn as_slice(&self) -> &[Environment] {
        &self.0
    }

    /// The environments offered, in order.
    pub fn into_vec(self) -> Vec<Environment> {
        self.0
    }

    /// The environments offered to clients in `Welcome`, with their versions.
    pub fn info(&self) -> Vec<EnvironmentInfo> {
        self.0
            .iter()
            .map(|environment| EnvironmentInfo {
                id: environment.id.clone(),
                label: environment.label.clone(),
                version: environment.version,
            })
            .collect()
    }

    /// The protocol version of the environment `id`, or `None` when it is not offered.
    pub fn version(&self, id: &str) -> Option<u32> {
        self.get(id).map(Environment::version)
    }

    /// Where the projection walls stand in a space in the environment `id`, in the space's coordinates: the Tomori wall
    /// at its home, the environment's `stage` anchor carried into them, if it is then finite, within
    /// [`crate::stage::REACH`] and has an orientation, made upright; or else beneath the agent's home, the environment's
    /// `agent` anchor carried into them as [`Self::anchors`] carries it, as `tomori_protocol::stance::wall_beneath`
    /// stands it; or else at [`crate::stage::default_stage`], beneath the agent's default home; and the environment's
    /// own walls, carried as it is, none when it is not offered. See `design/projection.md`.
    pub(crate) fn walls<'a>(&'a self, id: &'a str) -> crate::stage::Walls<'a> {
        let environment = self.get(id);
        crate::stage::Walls {
            environment: id,
            tomori: environment
                .and_then(Environment::tomori_wall_home)
                .unwrap_or_else(crate::stage::default_stage),
            declared: environment.map_or(&[], |environment| &environment.carried),
            status: environment.map_or(crate::stage::Status::Beneath, Environment::status),
        }
    }

    /// The avatar the environment `id` names for its agent, as a space's state carries it, if it names one clients read,
    /// and none when it is not offered. See `design/avatars.md`.
    pub fn avatar(&self, id: &str) -> Option<String> {
        self.get(id)
            .and_then(Environment::avatar)
            .filter(|avatar| tomori_protocol::valid_avatar(avatar))
            .map(str::to_owned)
    }

    /// How high above the space's floor the agent of the environment `id` hovers as it comes to someone, if it says.
    pub(crate) fn follow_height(&self, id: &str) -> Option<f32> {
        self.get(id).and_then(Environment::follow_height)
    }

    /// The walkable ground of the environment `id`, carried into a space's coordinates as its solids are, if it gives
    /// it, none when it is not offered.
    pub(crate) fn ground(&self, id: &str) -> Option<crate::follow::Ground> {
        self.get(id).and_then(|environment| {
            environment
                .navmesh
                .clone()
                .map(|mesh| crate::follow::Ground::new(mesh, environment.carriage()))
        })
    }

    /// The solids of the environment `id`, carried into a space's coordinates, none when it is not offered.
    pub(crate) fn solids(&self, id: &str) -> Vec<tomori_environment::sightlines::Solid> {
        self.get(id)
            .map_or_else(Vec::new, Environment::carried_solids)
    }

    /// `pose`, in the environment `id`'s own coordinates, carried into a space's, as its anchors are: finite, within
    /// [`crate::stage::REACH`] of home and with an orientation, made upright; or none, as for one it does not offer.
    pub(crate) fn carry(
        &self,
        id: &str,
        pose: &tomori_protocol::Pose,
    ) -> Option<tomori_protocol::Pose> {
        self.get(id)?.carried(Some(pose))
    }

    /// `pose`, in a space's coordinates, in the environment `id`'s own: carried back by its carriage, turned and all, as
    /// a participant's head and hands are for its server side; or as it is, for one that gives no arrival or that it
    /// does not offer, which runs no side to be told.
    pub(crate) fn uncarry(&self, id: &str, pose: &tomori_protocol::Pose) -> tomori_protocol::Pose {
        match self.get(id).and_then(Environment::carriage) {
            None => *pose,
            Some(carriage) => uncarried(&carriage, pose),
        }
    }

    /// The names of the cluster anchors the environment `id` declares, `home` always among them, since the place is
    /// the environment's whether or not it gives it.
    pub(crate) fn declared(&self, id: &str) -> Vec<String> {
        let mut names = vec![tomori_protocol::cluster::HOME.to_owned()];
        names.extend(
            self.get(id)
                .into_iter()
                .flat_map(|environment| &environment.anchors)
                .filter(|anchor| !anchor.is_home())
                .map(|anchor| anchor.name.clone()),
        );
        names
    }

    /// Where the environment `id` has participants arrive and puts the agent's home and the workers' arrangement, as a
    /// space's state carries them: its arrival, in its own coordinates, when it gives a valid one; and the agent and
    /// workers anchors it gives, carried into the space's coordinates, those then finite, within
    /// [`crate::stage::REACH`] of home and with an orientation, each made upright, with its `home` cluster anchor
    /// carried there too where it gives its room, and its other cluster anchors, carried and bounded alike; none when it
    /// is not offered. See `design/environments.md`.
    pub fn anchors(&self, id: &str) -> tomori_protocol::Anchors {
        let mut anchors = tomori_protocol::Anchors::new();
        if let Some(environment) = self.get(id) {
            anchors.agent = environment.carried(environment.agent.as_ref());
            anchors.workers = environment.carried(environment.workers().as_ref());
            // The home anchor's room, formation or ring, where it gives one, beside the workers anchor an older client
            // keeps.
            if let Some(home) = environment.home()
                && (home.extent.is_some() || !home.formation.is_arc() || !home.ring.is_floor())
                && let Some(pose) = anchors.workers
            {
                let mut carried = home.clone();
                carried.pose = pose;
                anchors.clusters.push(carried);
            }
            // Its other anchors, each carried and bounded as the agent's home is, or left out.
            for anchor in environment
                .anchors
                .iter()
                .filter(|anchor| !anchor.is_home())
            {
                if let Some(pose) = environment.carried(Some(&anchor.pose)) {
                    let mut carried = anchor.clone();
                    carried.pose = pose;
                    anchors.clusters.push(carried);
                }
            }
            anchors.arrival = environment
                .arrival
                .as_ref()
                .and_then(crate::stage::valid_arrival);
            // Where people arrive, each carried and bounded as the agent's home is, or left out.
            anchors.spawns = environment
                .spawns
                .iter()
                .filter_map(|spawn| {
                    let mut carried = spawn.clone();
                    carried.pose = environment.carried(Some(&spawn.pose))?;
                    Some(carried)
                })
                .collect();
        }
        anchors
    }

    /// The environments offered to a client that `presents` these, in order: those it lists with the same identifier and version, or every one for a client that lists none, as one from before versions. See `design/environments.md`.
    pub fn presentable(&self, presents: Option<&[EnvironmentVersion]>) -> Self {
        match presents {
            None => self.clone(),
            Some(presents) => Self(
                self.0
                    .iter()
                    .filter(|environment| presented(presents, environment))
                    .cloned()
                    .collect(),
            ),
        }
    }

    /// The environment to bind a new space to, or change one to, for a client that `presents` these: as [`Self::choose`] among those offered to it, saying so when the server offers it none, and naming the version of one the server offers that the client does not present at it.
    pub fn choose_for(
        &self,
        requested: Option<&str>,
        presents: Option<&[EnvironmentVersion]>,
    ) -> Result<String, String> {
        let offered = self.presentable(presents);
        if offered.0.is_empty() && !self.0.is_empty() {
            return Err("The server offers no environment this client can present.".to_owned());
        }
        // The server's own identifier and version, never the request's text, so that the refusal says why.
        if let Some(environment) = requested
            .and_then(|requested| self.get(requested))
            .filter(|environment| offered.get(&environment.id).is_none())
        {
            return Err(format!(
                "This client cannot present {} at version {}; available: {}.",
                environment.id,
                environment.version,
                offered.ids().join(", ")
            ));
        }
        offered.choose(requested)
    }

    /// The identifiers offered, in order.
    fn ids(&self) -> Vec<&str> {
        self.0
            .iter()
            .map(|environment| environment.id.as_str())
            .collect()
    }

    /// What the environment `id` tells the agent about what it presents and how to use it, its layer of a space's instructions; see `design/spaces.md`. An environment no longer offered, such as one a kept space was bound to by a server built with other environments, contributes none.
    pub fn instructions(&self, id: &str) -> &str {
        self.0
            .iter()
            .find(|environment| environment.id == id)
            .map_or("", Environment::instructions)
    }

    /// The environment to bind a new space to: `requested`, when the server offers it, or the first offered when the request names none, as from a client that predates environments. Refusals name what is offered and never repeat the request.
    pub fn choose(&self, requested: Option<&str>) -> Result<String, String> {
        let found = match requested {
            Some(requested) => self
                .0
                .iter()
                .find(|environment| environment.id == requested),
            None => self.0.first(),
        };
        match found {
            Some(environment) => Ok(environment.id.clone()),
            None if self.0.is_empty() => Err("The server offers no environments.".to_owned()),
            None => Err(format!(
                "Unknown environment; available: {}.",
                self.ids().join(", ")
            )),
        }
    }
}

/// Whether a client that `presents` these presents `environment` at its version.
fn presented(presents: &[EnvironmentVersion], environment: &Environment) -> bool {
    presents
        .iter()
        .any(|presented| presented.id == environment.id && presented.version == environment.version)
}

#[cfg(test)]
mod tests {
    use super::*;

    use tomori_environment::Space;

    /// Environments standing in for an application's, as Tomori's own server offers Sky and the grid.
    const SKY: &str = "sky";
    const MINIMAL: &str = tomori_protocol::GRID;

    /// What an application offers, as Tomori's own server does.
    fn tomoris() -> Environments {
        Environments::new([
            Environment::new(SKY, "Sky"),
            Environment::new(MINIMAL, "Grid"),
        ])
    }

    struct Idle;

    impl ServerSide for Idle {
        fn run(&self, _: &Space) -> tomori_environment::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_wall_is_named_by_letters_digits_spaces_and_hyphens_that_the_agent_can_say_in_a_place() {
        for good in [
            "Details",
            "Pods under load",
            "a",
            "Zone-2",
            &"n".repeat(WALL_NAME_BYTES),
        ] {
            assert!(valid_wall_name(good), "{good:?}");
        }
        let long = "n".repeat(WALL_NAME_BYTES + 1);
        for bad in [
            "",
            " Details",
            "Details ",
            "Two  spaces",
            "Café",
            "Details!",
            "a/b",
            "the Details",
            "Slot machine",
            "Pod slot",
            "Tomori wall",
            "environment wall 2",
            "Added wall",
            "wall-3",
            "near the door",
            "Nearest",
            "Look here",
            "looking glass",
            "Front desk",
            "BESIDE",
            "Wall of fame",
            &long,
        ] {
            assert!(!valid_wall_name(bad), "{bad:?}");
        }
        // A word that only holds `slot`, or a place's first word, is fine.
        assert!(valid_wall_name("Slots"));
        assert!(valid_wall_name("Nearby"));
        assert!(valid_wall_name("Frontline"));
        assert!(valid_wall_name("Pods near load"));
    }

    #[test]
    fn an_environment_has_a_server_side_only_when_given_one() {
        crate::test_log::record();
        let plain = Environment::new("observe", "Observe");
        let with = Environment::new("observe", "Observe").with_server_side(Idle);
        assert!(plain.server_side().is_none());
        assert!(with.server_side().is_some());
        assert!(format!("{plain:?}").contains("server_side: false"));
        assert!(format!("{with:?}").contains("server_side: true"));

        // Equal when they offer the same side, not one like it, or both offer none.
        assert_eq!(plain, Environment::new("observe", "Observe"));
        assert_eq!(with, with.clone());
        assert_ne!(
            with,
            Environment::new("observe", "Observe").with_server_side(Idle)
        );
        assert_ne!(plain, with);
        assert_ne!(with, plain);
        assert_ne!(plain, Environment::new("observe", "Other"));
        // And when their projection walls stand in the same places.
        let pose = tomori_protocol::Pose::default();
        assert_ne!(plain, plain.clone().with_stage(pose));
        assert_ne!(plain, plain.clone().with_walls([pose]));
        assert!(
            format!("{:?}", plain.clone().with_walls([pose]))
                .contains("walls: [EnvironmentWall { pose: Pose")
        );
        // A wall's own grid and slots' size, within bounds, or else a flat wall's.
        let laid = |wall: EnvironmentWall| (wall.layout().grid, wall.layout().scale);
        let flat = crate::stage::Layout::FLAT;
        assert_eq!(
            laid(
                EnvironmentWall::new(pose)
                    .with_grid(tomori_protocol::Grid::from((2, 1)))
                    .with_scale(1.5)
            ),
            (tomori_protocol::Grid::from((2, 1)), 1.5)
        );
        for scale in [0.2, 6.0, f32::NAN] {
            assert_eq!(
                laid(EnvironmentWall::new(pose).with_scale(scale)),
                (flat.grid, flat.scale)
            );
        }
        assert_eq!(
            EnvironmentWall::new(pose)
                .with_grid(tomori_protocol::Grid::from((9, 4)))
                .grid(),
            flat.grid
        );
        assert_ne!(
            plain.clone().with_walls([pose]),
            plain.clone().with_walls([
                EnvironmentWall::new(pose).with_grid(tomori_protocol::Grid::from((2, 1)))
            ])
        );
        // Its slots' shape, its curve and its tilt, within bounds, or else a flat wall's: standard, flat and upright.
        let console = EnvironmentWall::new(pose)
            .with_grid(tomori_protocol::Grid::from((1, 3)))
            .with_aspect(4.0)
            .with_curve(5.0)
            .with_tilt(-15.0);
        assert_eq!(
            console.layout(),
            crate::stage::Layout {
                grid: tomori_protocol::Grid::from((1, 3)),
                aspect: Some(4.0),
                radius: Some(5.0),
                tilt: Some(-15.0),
                ..flat
            }
        );
        // A name the server takes, and one it does not, and the second of two alike, in any case, is nobody's.
        let named = |name: &'static str| EnvironmentWall::new(pose).with_name(name);
        assert_eq!(named("Details").name(), Some("Details"));
        assert_eq!(named("Details").layout(), flat);
        assert_eq!(EnvironmentWall::new(pose).name(), None);
        assert_eq!(named("Tomori wall").name(), None);
        let env = plain
            .clone()
            .with_walls([named("Details"), named("details"), named("Load")]);
        let names: Vec<_> = env.walls().iter().map(EnvironmentWall::name).collect();
        assert_eq!(names, [Some("Details"), None, Some("Load")]);
        // A name the server does not take takes nothing from a later wall's, which is still kept from its own double.
        let env = plain
            .clone()
            .with_walls([named("Tomori"), named("Details"), named("DETAILS")]);
        let names: Vec<_> = env.walls().iter().map(EnvironmentWall::name).collect();
        assert_eq!(names, [None, Some("Details"), None]);
        // Carried into a space, a wall keeps its name.
        let carried = plain
            .clone()
            .with_arrival(tomori_protocol::Pose::at([1.0, 0.0, 0.0]))
            .with_walls([
                EnvironmentWall::new(tomori_protocol::Pose::at([0.0, 1.5, -3.0]))
                    .with_name("Details"),
            ]);
        assert_eq!(carried.carried[0].name(), Some("Details"));
        // Upright is no tilt, either side of zero.
        for upright in [0.0, -0.0] {
            assert_eq!(EnvironmentWall::new(pose).with_tilt(upright).tilt(), None);
        }
        // As a participant's change must, a declared wall keeps within a wall's size and curves no further round than
        // half a circle; one that would not is laid out without its shape, curve and tilt, or else as a flat wall.
        let upright = tomori_protocol::Pose::at([0.0, 1.5, -3.0]);
        let wide = EnvironmentWall::new(upright)
            .with_grid(tomori_protocol::Grid::from((8, 1)))
            .with_scale(2.0)
            .with_aspect(4.0)
            .with_tilt(10.0);
        let (laid, why) = wide.checked();
        assert!(
            why.unwrap().starts_with("A wall is at most 16 m wide"),
            "{why:?}"
        );
        assert_eq!(
            laid,
            crate::stage::Layout {
                grid: tomori_protocol::Grid::from((8, 1)),
                scale: 2.0,
                ..flat
            }
        );
        let bent = EnvironmentWall::new(upright)
            .with_grid(tomori_protocol::Grid::from((3, 1)))
            .with_aspect(4.0)
            .with_curve(1.0);
        assert!(bent.checked().1.unwrap().starts_with("Curved that tightly"));
        assert_eq!((bent.radius(), bent.aspect()), (None, None));
        let huge = EnvironmentWall::new(upright)
            .with_grid(tomori_protocol::Grid::from((8, 1)))
            .with_scale(5.0);
        assert_eq!(huge.layout(), flat);
        assert!(huge.checked().1.is_some());
        // Declared so, the environment still registers it, laid out as it can be, saying why in the log.
        let registered = plain.clone().with_walls([wide, bent, huge]);
        assert_eq!(registered.walls().len(), 3);
        assert_eq!(registered.walls()[2].layout(), flat);
        for (aspect, radius, tilt) in [
            (0.1, 0.5, 60.0),
            (9.0, 80.0, -50.0),
            (f32::NAN, f32::INFINITY, 0.0),
        ] {
            let wall = EnvironmentWall::new(pose)
                .with_aspect(aspect)
                .with_curve(radius)
                .with_tilt(tilt);
            assert_eq!(
                (wall.aspect(), wall.radius(), wall.tilt()),
                (None, None, None)
            );
        }
        assert!(!crate::test_log::logged("is laid out as a flat wall's").is_empty());
    }

    #[test]
    fn anchors_are_sent_upright_and_within_reach_and_move_the_tomori_wall() {
        use tomori_protocol::{Anchors, Pose};
        let pod = Pose {
            position: [2.0, 1.6, -4.0],
            orientation: [0.0, 0.0, 0.0, 1.0],
        };
        // Tilted, it is made upright, as walls are.
        let tilted = Pose {
            position: [0.0, 0.0, -6.0],
            orientation: [0.0, 0.0, 0.38268343, 0.9238795],
        };
        let far = Pose {
            position: [0.0, 0.0, -31.0],
            ..pod
        };
        let offered = tomoris()
            .with(
                Environment::new("observe", "Observe")
                    .with_agent(pod)
                    .with_workers(tilted),
            )
            .with(
                Environment::new("far", "Far")
                    .with_agent(far)
                    .with_workers(Pose::default()),
            )
            .with(
                Environment::new("staged", "Staged")
                    .with_agent(pod)
                    .with_stage(tilted),
            )
            .with(
                Environment::new("unseen", "Unseen")
                    .with_agent(Pose {
                        position: [f32::NAN, 1.0, -3.0],
                        ..pod
                    })
                    .with_workers(Pose {
                        position: [0.0, f32::INFINITY, -3.0],
                        ..pod
                    })
                    .with_stage(far),
            );
        let observe = offered.get("observe").unwrap();
        assert_eq!(
            (observe.agent(), observe.workers()),
            (Some(pod), Some(tilted))
        );
        assert!(format!("{observe:?}").contains("agent: Some"));
        let anchors = offered.anchors("observe");
        assert_eq!(anchors.agent, Some(pod));
        let workers = anchors.workers.unwrap();
        assert_eq!(workers.position, tilted.position);
        assert_eq!((workers.orientation[0], workers.orientation[2]), (0.0, 0.0));
        // Beyond reach, or without an orientation, left out; and none for an environment without them or not offered.
        assert_eq!(offered.anchors("far"), Anchors::new());
        assert_eq!(offered.anchors(SKY), Anchors::new());
        assert_eq!(offered.anchors("unknown"), Anchors::new());
        // The Tomori wall's home is beneath the agent's home, wherever that is, facing the way it faces, unless the
        // environment gives one of its own.
        let beneath = offered.walls("observe").tomori;
        assert_eq!(
            beneath,
            tomori_protocol::stance::wall_beneath(&pod, tomori_protocol::cluster::FLOOR)
        );
        assert_eq!(
            [beneath.position[0], beneath.position[2]],
            [pod.position[0], pod.position[2]]
        );
        assert!((pod.position[1] - beneath.position[1] - 0.88).abs() < 1e-5);
        assert_eq!(beneath.orientation, pod.orientation);
        // A stage is made upright too, and one beyond reach is left out, as an anchor not finite is: then beneath the
        // agent, or, with neither, the default, beneath the default agent.
        let staged = offered.walls("staged").tomori;
        assert_eq!(staged.position, tilted.position);
        assert_eq!((staged.orientation[0], staged.orientation[2]), (0.0, 0.0));
        assert_eq!(offered.anchors("unseen"), Anchors::new());
        assert_eq!(
            offered.walls("unseen").tomori,
            crate::stage::default_stage()
        );
        assert_eq!(offered.walls("far").tomori, crate::stage::default_stage());
        assert_eq!(offered.walls(SKY).tomori, crate::stage::default_stage());
        let default_agent = Pose::at(crate::stage::AGENT).facing_towards([0.0, 1.4, 0.0]);
        let default_beneath =
            tomori_protocol::stance::wall_beneath(&default_agent, tomori_protocol::cluster::FLOOR);
        let default = crate::stage::default_stage();
        assert!(
            default_beneath
                .position
                .iter()
                .zip(default.position)
                .all(|(a, b)| (a - b).abs() < 1e-5)
        );
        assert_eq!(default_beneath.orientation, default.orientation);
        // An agent anchor too low for its wall beneath it keeps the wall clear of the floor.
        let low = Pose::at([0.0, 0.8, -3.0]).facing_towards([0.0, 0.8, 0.0]);
        let offered = offered.with(Environment::new("low", "Low").with_agent(low));
        assert_eq!(
            offered.walls("low").tomori.position[1],
            tomori_protocol::stance::LOWEST_WALL
        );
        // Anchors tell environments apart.
        let plain = Environment::new("observe", "Observe");
        assert_ne!(plain, plain.clone().with_agent(pod));
        assert_ne!(plain, plain.clone().with_workers(pod));
    }

    #[test]
    fn the_home_anchor_gives_the_workers_room_and_with_workers_is_its_compatible_form() {
        crate::test_log::record();
        use tomori_protocol::Pose;
        use tomori_protocol::cluster::{Anchor, Extent, HOME};
        let yard = Pose::at([0.0, 1.4, -5.0]).facing_towards([0.0, 1.6, 0.0]);
        let room = Anchor::new(HOME, yard).with_extent(6.0, 2.5, 3.0);
        // `with_workers` declares a home anchor of today's arc's room, in place of one before; the home anchor's room
        // is sent beside the workers anchor, carried as it is.
        let arrival = Pose::at([0.0, 0.0, 8.0]).facing_towards([0.0, 0.0, 0.0]);
        let offered = tomoris()
            .with(Environment::new("roomy", "Roomy").with_anchor(room.clone()))
            .with(
                Environment::new("compatible", "Compatible")
                    .with_anchor(room.clone())
                    .with_workers(yard),
            )
            .with(
                Environment::new("street", "Street")
                    .with_arrival(arrival)
                    .with_anchor(room.clone()),
            );
        let roomy = offered.anchors("roomy");
        assert_eq!(roomy.workers, Some(yard));
        assert_eq!(roomy.clusters, std::slice::from_ref(&room));
        let compatible = offered.get("compatible").unwrap();
        assert_eq!(compatible.anchors(), [Anchor::new(HOME, yard)]);
        assert_eq!(compatible.home(), Some(&Anchor::new(HOME, yard)));
        assert_eq!(Environment::new("plain", "Plain").home(), None);
        assert_eq!(compatible.workers(), Some(yard));
        let compatible = offered.anchors("compatible");
        assert_eq!(
            (compatible.workers, compatible.clusters),
            (Some(yard), vec![])
        );
        // A home anchor that stands its workers in a ring is sent, with or without room, its sweep with it; one that
        // stands them on the arc, without room, is not, as `with_workers` declares it.
        use tomori_protocol::cluster::Formation;
        let ring = Anchor::new(HOME, yard)
            .with_formation(Formation::Ring)
            .with_sweep(240.0);
        let ringed = tomoris().with(Environment::new("ringed", "Ringed").with_anchor(ring.clone()));
        assert_eq!(ringed.anchors("ringed").clusters, [ring]);
        assert_eq!(ringed.anchors("ringed").workers, Some(yard));
        // So is one on the arc, without room, whose ring lies under the home.
        let under = Anchor::new(HOME, yard).with_ring(tomori_protocol::cluster::RingAt::UnderHome);
        let floating =
            tomoris().with(Environment::new("floating", "Floating").with_anchor(under.clone()));
        assert_eq!(floating.anchors("floating").clusters, [under]);
        let street = offered.anchors("street");
        let home = street.cluster(HOME).unwrap();
        assert_eq!(Some(home.pose), street.workers);
        assert!((home.pose.position[2] + 13.0).abs() < 1e-4, "{home:?}");
        assert_eq!(home.extent, Some(Extent::new(6.0, 2.5, 3.0)));
        // One of a name already given, or of a name or room it may not have, is left out; another anchor is sent, carried
        // and bounded as home is, its label cleaned and cut, never said to be placed as it runs.
        let aisle = Anchor::new("aisle-3", yard).with_extent(4.0, 2.5, 1.5);
        let mut claimed = Anchor::new("rack/a12", yard)
            .with_kind("rack")
            .with_label(format!("Rack A12,\nalerting {}", "!".repeat(200)));
        claimed.placed = true;
        let checked = Environment::new("checked", "Checked")
            .with_anchor(room.clone())
            .with_anchor(Anchor::new(HOME, Pose::HOME))
            .with_anchor(Anchor::new("rack a12", yard))
            .with_anchor(Anchor::new("cupboard", yard).with_extent(0.1, 1.0, 1.0))
            .with_anchor(Anchor::new("tomori/near/1", yard))
            .with_anchor(aisle.clone())
            .with_anchor(claimed)
            .with_anchor(Anchor::new("beyond", Pose::at([0.0, 0.0, -35.0])));
        let rack = &checked.anchors()[2];
        let label = rack.label.as_deref().unwrap();
        assert!(
            label.starts_with("Rack A12, alerting") && !label.contains('\n'),
            "{label}"
        );
        // Cut after its limit, marked cut.
        assert!(label.ends_with('…'));
        assert_eq!(
            label.chars().count(),
            tomori_environment::ANCHOR_LABEL_LIMIT + 1
        );
        assert!(!rack.placed);
        let rack = rack.clone();
        assert_eq!(checked.anchors()[..2], [room.clone(), aisle.clone()]);
        assert!(format!("{checked:?}").contains("aisle-3"));
        let offered = offered.with(checked);
        assert_eq!(
            offered.anchors("checked").clusters,
            [room.clone(), aisle, rack]
        );
        // The names it declares, home always among them, even where it gives none.
        assert_eq!(
            offered.declared("checked"),
            ["home", "aisle-3", "rack/a12", "beyond"]
        );
        assert_eq!(offered.declared("roomy"), ["home"]);
        assert_eq!(offered.declared("unknown"), ["home"]);
        // A label of nothing but space is none.
        let blank = Environment::new("blank", "Blank")
            .with_anchor(Anchor::new("a", yard).with_label(" \n "));
        assert_eq!(blank.anchors()[0].label, None);
        // At most 64.
        let many = (0..70).fold(Environment::new("many", "Many"), |many, index| {
            many.with_anchor(Anchor::new(format!("rack/{index}"), yard))
        });
        assert_eq!(many.anchors().len(), tomori_environment::ANCHOR_LIMIT);
        // A pose a side gives is carried as the environment's are, or none beyond reach, or for one not offered.
        assert_eq!(
            offered
                .carry("street", &Pose::at([0.0, 1.0, -5.0]))
                .map(|pose| pose.position[2]),
            Some(-13.0)
        );
        assert_eq!(offered.carry("street", &Pose::at([0.0, 1.0, -40.0])), None);
        assert_eq!(offered.carry("unknown", &Pose::HOME), None);
        // Beyond reach, it is left out, as the workers anchor is.
        let far = Anchor::new(HOME, Pose::at([0.0, 0.0, -35.0])).with_extent(6.0, 2.5, 3.0);
        let offered = offered.with(Environment::new("far", "Far").with_anchor(far));
        assert_eq!(offered.anchors("far"), tomori_protocol::Anchors::new());
        let plain = Environment::new("roomy", "Roomy");
        assert_ne!(plain, plain.clone().with_anchor(room));
        assert!(!crate::test_log::logged("'s anchor ").is_empty());
    }

    #[test]
    fn spawn_points_are_carried_into_the_space_and_bounded() {
        crate::test_log::record();
        use tomori_protocol::Pose;
        use tomori_protocol::spawn::{self, Spawn};
        let gate = Spawn::new(Pose::at([0.0, 0.0, 6.0]).facing_towards([0.0, 0.0, 0.0]))
            .with_tag(spawn::VISITOR);
        let camp = Spawn::new(Pose::at([4.0, 0.5, -2.0]));
        // Arriving 8 m along +Z, facing −Z: the gate stands 2 m behind home, the campsite 10 m ahead.
        let arrival = Pose::at([0.0, 0.0, 8.0]).facing_towards([0.0, 0.0, 0.0]);
        let farm = Environment::new("farm", "Farm")
            .with_arrival(arrival)
            .with_spawn(gate.clone())
            .with_spawn(camp.clone())
            // Refused: a tag that is no slug, a pose that is not finite, one without an orientation, and one beyond
            // reach once carried, which the space leaves out.
            .with_spawn(Spawn::new(Pose::HOME).with_tag("by the gate"))
            .with_spawn(Spawn::new(Pose::at([f32::NAN, 0.0, 0.0])))
            .with_spawn(Spawn::new(Pose {
                position: [0.0; 3],
                orientation: [0.0; 4],
            }))
            .with_spawn(Spawn::new(Pose::at([0.0, 0.0, -30.0])));
        assert_eq!(farm.spawns().len(), 3);
        assert_eq!(farm.spawns()[..2], [gate.clone(), camp.clone()]);
        assert!(format!("{farm:?}").contains("spawns"));
        let offered = tomoris().with(farm);
        let spawns = offered.anchors("farm").spawns;
        assert_eq!(spawns.len(), 2);
        assert_eq!(spawns[0].tags, [spawn::VISITOR]);
        assert!(
            spawns[0]
                .pose
                .position
                .iter()
                .zip([0.0, 0.0, -2.0])
                .all(|(a, b)| (a - b).abs() < 1e-4),
            "{spawns:?}"
        );
        assert!(
            (spawns[1].pose.position[2] + 10.0).abs() < 1e-4,
            "{spawns:?}"
        );
        assert!((spawns[1].pose.position[1] - 0.5).abs() < 1e-4);
        // At most 32.
        let many = (0..40).fold(Environment::new("many", "Many"), |many, index| {
            many.with_spawn(Spawn::new(Pose::at([index as f32 * 0.1, 0.0, 0.0])))
        });
        assert_eq!(many.spawns().len(), spawn::SPAWN_LIMIT);
        // An environment declaring none sends none, and spawn points tell environments apart.
        assert!(offered.anchors(SKY).spawns.is_empty());
        let plain = Environment::new("farm", "Farm");
        assert_ne!(plain, plain.clone().with_spawn(camp));
        assert!(!crate::test_log::logged("spawn point").is_empty());
    }

    #[test]
    fn an_arrival_carries_the_environment_s_anchors_and_walls_into_the_space_s_coordinates() {
        use tomori_protocol::Pose;
        let close = |a: Pose, b: Pose| {
            a.position
                .iter()
                .zip(b.position)
                .all(|(a, b)| (a - b).abs() < 1e-4)
                && (a.yaw() - b.yaw()).sin().abs() < 1e-4
                && (a.yaw() - b.yaw()).cos() > 0.0
        };
        // A street 8 m along +X from the environment's origin, raised 0.5 m, looking along −X at a balcony.
        let street = Pose::at([8.0, 0.5, 0.0]).facing_towards([0.0, 0.5, 0.0]);
        let balcony = Pose::at([0.0, 4.0, 0.0]).facing_towards(street.position);
        let wall = Pose::at([6.0, 1.7, -1.0]).facing_towards(street.position);
        let board = Pose::at([4.0, 2.0, 2.0]).facing_towards(street.position);
        let offered = tomoris().with(
            Environment::new("town", "Town")
                .with_walls([board])
                .with_arrival(street)
                .with_agent(balcony)
                .with_stage(wall),
        );
        let town = offered.get("town").unwrap();
        assert_eq!(town.arrival(), Some(street));
        // Given in its own coordinates, they are kept as given.
        assert_eq!(
            (town.agent(), town.stage(), town.walls()),
            (
                Some(balcony),
                Some(wall),
                &[EnvironmentWall::from(board)][..]
            )
        );
        let anchors = offered.anchors("town");
        assert!(close(anchors.arrival.unwrap(), street));
        // Home is the origin, facing −Z: the balcony stands 8 m ahead, 3.5 m above the floor where people arrive, facing
        // home; and the workers stand about it.
        assert!(close(
            anchors.agent.unwrap(),
            Pose::at([0.0, 3.5, -8.0]).facing_towards([0.0, 3.5, 0.0])
        ));
        assert_eq!(anchors.workers, None);
        // The Tomori wall stands at its home, 2.2 m ahead and 1 m to the right, facing home, and the board as carried.
        let walls = offered.walls("town");
        assert!(close(
            walls.tomori,
            Pose::at([1.0, 1.2, -2.0]).facing_towards([0.0, 1.2, 0.0])
        ));
        assert!(close(
            walls.declared[0].pose,
            Pose::at([-2.0, 1.5, -4.0]).facing_towards([0.0, 1.5, 0.0])
        ));
        // Without its own, it stands beneath the balcony, carried, above the floor where people arrive.
        let beneath = tomoris().with(
            Environment::new("town", "Town")
                .with_arrival(street)
                .with_agent(balcony),
        );
        assert!(close(
            beneath.walls("town").tomori,
            Pose::at([0.0, 3.5 - 0.88, -8.0]).facing_towards([0.0, 2.62, 0.0])
        ));
        // Without an arrival, nothing moves; an arrival not finite is ignored, as none.
        let plain = Environment::new("plain", "Plain")
            .with_walls([board])
            .with_agent(balcony);
        let unseen = plain.clone().with_arrival(Pose {
            position: [f32::NAN, 0.0, 0.0],
            ..street
        });
        let offered = tomoris().with(plain).with(Environment {
            id: "unseen".into(),
            ..unseen
        });
        for id in ["plain", "unseen"] {
            assert_eq!(offered.walls(id).declared, [board]);
            assert!(close(offered.anchors(id).agent.unwrap(), balcony));
            assert_eq!(offered.anchors(id).arrival, None);
        }
        // An arrival beyond a kilometre is ignored too; and an anchor carried beyond 30 m of home is left out.
        let far = Environment::new("far", "Far")
            .with_arrival(Pose::at([0.0, 0.0, 40.0]))
            .with_agent(balcony)
            .with_workers(Pose::at([f32::NAN, 0.0, 0.0]))
            .with_walls([Pose::at([f32::NAN, 0.0, 0.0])]);
        assert!(far.carriage().is_some());
        let offered = tomoris().with(far);
        assert_eq!(offered.anchors("far").agent, None);
        // One not finite is left out before it is carried, as a wall such as that is.
        assert_eq!(offered.anchors("far").workers, None);
        assert!(offered.walls("far").declared.is_empty());
        assert_eq!(
            Environment::new("away", "Away")
                .with_arrival(Pose::at([2000.0, 0.0, 0.0]))
                .carriage(),
            None
        );
    }

    #[test]
    fn an_environment_s_solids_are_carried_into_a_space_and_bounded() {
        use tomori_environment::sightlines::Solid;
        let shed = ([1.0, 0.0, -4.0], [3.0, 2.0, -3.0]);
        let plain = Environment::new("field", "Field")
            .with_solids([shed, ([f32::NAN, 0.0, 0.0], [1.0; 3])]);
        assert_eq!(plain.solids().len(), 2);
        // Arriving at its origin, facing −Z, its solids stand where they are; one not finite is left out.
        assert_eq!(plain.carried_solids(), [Solid::new(shed.0, shed.1)]);
        // Arriving 2 m along +X, facing +X, they are carried, each the box about its corners.
        let turned = plain.clone().with_arrival(
            tomori_protocol::Pose::at([2.0, 0.0, 0.0]).facing_towards([5.0, 0.0, 0.0]),
        );
        let carried = turned.carried_solids();
        let expected = Solid::new([-4.0, 0.0, -1.0], [-3.0, 2.0, 1.0]);
        assert!(
            (0..3).all(
                |axis| (carried[0].low[axis] - expected.low[axis]).abs() < 1e-4
                    && (carried[0].high[axis] - expected.high[axis]).abs() < 1e-4
            ),
            "{carried:?}"
        );
        // Beyond the arrival's reach, left out too; and an environment not offered has none.
        let far =
            Environment::new("far", "Far").with_solids([([2000.0, 0.0, 0.0], [2001.0, 1.0, 1.0])]);
        assert!(far.carried_solids().is_empty());
        let offered = tomoris().with(turned);
        assert_eq!(offered.solids("field").len(), 1);
        assert!(offered.solids("nowhere").is_empty());
    }

    #[test]
    fn a_follow_height_is_taken_from_1_to_10_m_and_otherwise_left_out() {
        crate::test_log::record();
        for metres in [FOLLOW_HEIGHTS.0, 5.1, FOLLOW_HEIGHTS.1] {
            let observe = Environment::new("observe", "Observe").with_follow_height(metres);
            assert_eq!(observe.follow_height(), Some(metres));
        }
        for metres in [0.99, 10.01, -2.0, f32::NAN, f32::INFINITY] {
            let refused = Environment::new("observe", "Observe").with_follow_height(metres);
            assert_eq!(refused.follow_height(), None, "{metres}");
            assert_eq!(refused, Environment::new("observe", "Observe"));
        }
        let floating = Environment::new("observe", "Observe").with_follow_height(5.1);
        assert_ne!(floating, Environment::new("observe", "Observe"));
        assert!(format!("{floating:?}").contains("follow_height: Some(5.1)"));
        let offered = tomoris().with(floating);
        assert_eq!(offered.follow_height("observe"), Some(5.1));
        assert_eq!(offered.follow_height("nowhere"), None);
        assert!(!crate::test_log::logged("follow height of").is_empty());
    }

    #[test]
    fn a_server_side_is_run_with_the_space() {
        let environment = Environment::new("observe", "Observe").with_server_side(Idle);
        let side = environment.server_side().expect("it has one");
        assert_eq!(side.run(&Space::new("space-1", "Fleet", "/")), Ok(()));
    }

    #[test]
    fn an_environment_says_where_the_tomori_wall_stands_only_when_it_does() {
        let stage = tomori_protocol::Pose::default();
        assert_eq!(Environment::new("observe", "Observe").stage(), None);
        assert_eq!(
            Environment::new("observe", "Observe")
                .with_stage(stage)
                .stage(),
            Some(stage)
        );
    }

    #[test]
    fn an_environment_names_its_status_wall_one_of_its_own_or_one_it_stands_or_else_the_default_stands()
     {
        crate::test_log::record();
        use crate::stage::{Layout, Status};
        use tomori_protocol::{Grid, Pose};
        let board = EnvironmentWall::new(Pose::at([3.0, 1.4, -4.0])).with_grid(Grid::from((2, 1)));
        let single =
            EnvironmentWall::new(Pose::at([-3.0, 1.4, -4.0])).with_grid(Grid::from((1, 1)));
        let farm = Environment::new("farm", "Farm")
            .with_status_wall(StatusWall::own(0))
            .with_walls([board, single]);
        assert_eq!(farm.status(), Status::Own(0));
        assert!(format!("{farm:?}").contains("Own(0)"));
        assert_ne!(
            farm,
            Environment::new("farm", "Farm").with_walls([board, single])
        );
        let offered = Environments::new([
            farm.clone(),
            // A wall of one slot, or one it does not declare, is left out, and the default stands.
            farm.clone().with_status_wall(StatusWall::own(1)),
            farm.clone().with_status_wall(StatusWall::own(2)),
            Environment::new("strip", "Strip").with_status_wall(StatusWall::at(board)),
            Environment::new("tiny", "Tiny").with_status_wall(StatusWall::at(single)),
            Environment::new("far", "Far")
                .with_status_wall(StatusWall::at(Pose::at([0.0, 1.4, -40.0]))),
        ]);
        let walls = offered.walls("farm");
        assert_eq!(
            (walls.status, walls.status_wall()),
            (Status::Own(0), "environment-0".to_owned())
        );
        assert_eq!(
            Environment::new("farm", "Farm")
                .with_walls([board, single])
                .with_status_wall(StatusWall::own(1))
                .status(),
            Status::Beneath
        );
        assert_eq!(
            farm.clone().with_status_wall(StatusWall::own(2)).status(),
            Status::Beneath
        );
        // Of sixty-five walls, the sixty-fourth may be its status wall, and the sixty-fifth, ignored, may not.
        let deck = Environment::new("deck", "Deck").with_walls(vec![board; 65]);
        assert_eq!(deck.walls().len(), 65);
        assert_eq!(
            deck.clone().with_status_wall(StatusWall::own(63)).status(),
            Status::Own(63)
        );
        assert_eq!(
            deck.with_status_wall(StatusWall::own(64)).status(),
            Status::Beneath
        );
        // One it stands, laid out as it says, at its pose; beyond reach, the default.
        let strip = offered.get("strip").unwrap();
        assert_eq!(strip.status(), Status::At(board.layout()));
        assert_eq!(strip.stage(), Some(board.pose));
        assert_eq!(StatusWall::at(board).index(), None);
        assert_eq!(StatusWall::own(1).index(), Some(1));
        assert_eq!(offered.walls("strip").tomori, board.pose);
        assert_eq!(offered.get("tiny").unwrap().status(), Status::Beneath);
        assert_eq!(offered.get("far").unwrap().status(), Status::Beneath);
        assert_eq!(offered.walls("far").tomori, crate::stage::default_stage());
        // A stage is a status wall it stands, six slots in a curved row, as the default is laid out.
        let staged = Environment::new("staged", "Staged").with_stage(board.pose);
        assert_eq!(staged.status(), Status::At(Layout::TOMORI));
        assert_eq!(
            staged
                .status_wall()
                .and_then(|status| status.wall())
                .map(|wall| wall.grid()),
            Some(Layout::TOMORI.grid)
        );
        // Not offered, it is the default.
        assert_eq!(offered.walls("gone").status, Status::Beneath);
        assert!(!crate::test_log::logged("status wall is left out").is_empty());
    }

    #[test]
    fn an_offered_environment_is_found_by_its_identifier() {
        let offered = tomoris();
        assert_eq!(offered.get("sky").map(Environment::label), Some("Sky"));
        assert!(offered.get("observe").is_none());
    }

    #[test]
    fn only_offered_environments_are_bound() {
        let offered = tomoris();
        assert_eq!(offered.info().len(), 2);
        assert_eq!(offered.choose(Some(SKY)), Ok(SKY.to_owned()));
        assert_eq!(offered.choose(Some(MINIMAL)), Ok(MINIMAL.to_owned()));
        // A space whose creator names none is bound to Sky.
        assert_eq!(offered.choose(None), Ok(SKY.to_owned()));
        let refused = offered.choose(Some("observe")).unwrap_err();
        assert_eq!(refused, "Unknown environment; available: sky, minimal.");
        assert!(offered.choose(Some("Minimal")).is_err());
        assert_eq!(
            Environments::new([]).choose(None),
            Err("The server offers no environments.".to_owned())
        );
    }

    #[test]
    fn a_client_is_offered_the_environments_it_presents_at_their_versions() {
        let offered = tomoris().with(Environment::new("observe", "Observe").with_version(2));
        assert_eq!(offered.version("observe"), Some(2));
        assert_eq!(offered.version(SKY), Some(1));
        assert_eq!(offered.version("unknown"), None);
        let versions: Vec<u32> = offered.info().iter().map(|info| info.version).collect();
        assert_eq!(versions, [1, 1, 2]);
        // Versions tell environments apart, as offering them again would.
        assert_ne!(
            Environment::new("observe", "Observe"),
            Environment::new("observe", "Observe").with_version(2)
        );
        assert!(format!("{:?}", offered.get("observe").unwrap()).contains("version: 2"));

        // A client from before versions lists nothing, and is offered every one.
        assert_eq!(offered.presentable(None), offered);
        let presents = [
            EnvironmentVersion::new("observe", 1),
            EnvironmentVersion::new(MINIMAL, 1),
        ];
        let ids: Vec<String> = offered
            .presentable(Some(&presents))
            .info()
            .into_iter()
            .map(|info| info.id)
            .collect();
        assert_eq!(ids, [MINIMAL]);
        // The first offered to it is its default, and one not offered to it is refused, naming the version the server
        // offers it at and those offered to the client, while one the server does not offer is unknown.
        assert_eq!(
            offered.choose_for(None, Some(&presents)),
            Ok(MINIMAL.to_owned())
        );
        assert_eq!(
            offered.choose_for(Some("observe"), Some(&presents)),
            Err("This client cannot present observe at version 2; available: minimal.".to_owned())
        );
        assert_eq!(
            offered.choose_for(Some("floor"), Some(&presents)),
            Err("Unknown environment; available: minimal.".to_owned())
        );
        assert_eq!(
            offered.choose_for(Some("observe"), None),
            Ok("observe".to_owned())
        );
        // Offered none, it is told so, while a server offering none says that as before.
        assert_eq!(
            offered.choose_for(None, Some(&[])),
            Err("The server offers no environment this client can present.".to_owned())
        );
        assert_eq!(
            Environments::new([]).choose_for(None, Some(&[])),
            Err("The server offers no environments.".to_owned())
        );
    }

    #[test]
    fn a_pose_in_a_space_is_carried_back_into_its_environment_s_coordinates() {
        let arrival = tomori_protocol::Pose::at([5.0, 0.0, 2.0]).facing_towards([6.0, 0.0, 2.0]);
        let environments = Environments::new([
            Environment::new("grid", "Grid"),
            Environment::new("yard", "Yard").with_arrival(arrival),
        ]);
        let pose = tomori_protocol::Pose {
            position: [1.0, 1.5, -2.0],
            orientation: [0.0, 0.0, 0.0, 1.0],
        };
        // An environment that gives no arrival leaves it where it is; one not offered has none.
        assert_eq!(environments.uncarry("grid", &pose), pose);
        assert_eq!(environments.uncarry("moon", &pose), pose);
        // Carried back, it is carried forth to where it was.
        let back = environments.uncarry("yard", &pose);
        let forth = environments.carry("yard", &back).unwrap();
        assert!((0..3).all(|axis| (forth.position[axis] - pose.position[axis]).abs() < 1e-4));
        assert!((forth.yaw() - pose.yaw()).abs() < 1e-4);
    }

    #[test]
    fn an_environment_offered_again_replaces_the_one_with_its_identifier() {
        let offered = tomoris()
            .with(Environment::new("observe", "Observe").with_instructions("Mind the topology."))
            .with(Environment::new(SKY, "Night").with_instructions("Mind the stars."));
        let labels: Vec<(String, String)> = offered
            .info()
            .into_iter()
            .map(|info| (info.id, info.label))
            .collect();
        assert_eq!(
            labels,
            [
                (SKY.to_owned(), "Night".to_owned()),
                (MINIMAL.to_owned(), "Grid".to_owned()),
                ("observe".to_owned(), "Observe".to_owned()),
            ]
        );
        let observe = Environment::new("observe", "Observe");
        assert_eq!((observe.id(), observe.label()), ("observe", "Observe"));
        assert_eq!(offered.instructions(SKY), "Mind the stars.");
        assert_eq!(offered.instructions("observe"), "Mind the topology.");
        assert_eq!(offered.instructions(MINIMAL), "");
        assert_eq!(offered.instructions("unknown"), "");
        assert_eq!(tomoris().instructions(SKY), "");
        // None is offered until an application registers it.
        assert_eq!(Environments::default(), Environments::new([]));
    }
}
