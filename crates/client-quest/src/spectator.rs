// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Read-only projection of the Quest's actual scene, received from the PC bridge. The headset stopped publishing
//! frames when it moved to the server, so the PC view waits until publishing returns there.
use serde::{Deserialize, Serialize};
use std::{
    sync::mpsc::{self, Receiver},
    time::Duration,
};

pub const AGENTS: usize = crate::swarm::AGENT_COUNT;
pub const VIDEO_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Frame {
    pub sequence: u64,
    /// Absent in older headset builds: retain their original Minimal scenery.
    #[serde(default)]
    pub environment: crate::environment::Environment,
    pub projection: [f32; 16],
    pub scene_to_space: [f32; 16],
    pub head: [f32; 16],
    pub view_aspect: f32,
    pub agents: Vec<[f32; 3]>,
    pub controller: Option<[f32; 3]>,
    /// Full grip poses, left then right; absent in older headset builds.
    #[serde(default)]
    pub controllers: [Option<crate::controller::Pose>; 2],
    /// Optional hand skeletons, left then right; absent in older senders.
    #[serde(default)]
    pub hands: [Option<crate::hands::Hand>; 2],
    pub attracting: bool,
    pub caption: String,
    pub recording_ms: Option<u64>,
    /// Observable conversation state; older senders retain the original swarm.
    #[serde(default)]
    pub interaction: crate::interaction::Visual,
    /// Worker simulation stays authoritative on Quest, including formation paths.
    #[serde(default)]
    pub worker_cubes: Vec<crate::delegation::Cube>,
    #[serde(default)]
    pub worker_stations: Vec<crate::delegation::Station>,
    #[serde(default)]
    pub workspace: crate::delegation::Workspace,
    #[serde(default)]
    pub presentation_page: usize,
    /// Zero-based page within the focused card; absent in older senders.
    #[serde(default)]
    pub presentation_content_page: usize,
}

impl Frame {
    pub fn valid(&self) -> bool {
        self.agents.len() == AGENTS
            && self.caption.len() <= 8192
            && self.view_aspect.is_finite()
            && (0.25..=4.0).contains(&self.view_aspect)
            && self.controllers.iter().flatten().all(|pose| pose.valid())
            && self.hands.iter().flatten().all(|hand| hand.valid())
            && self.interaction.valid()
            && self.workspace.valid()
            && self.worker_cubes.len() <= crate::delegation::MAX_CUBES
            && self.worker_cubes.iter().all(|cube| cube.valid())
            && self.worker_stations.len() <= 128
            && self.worker_stations.iter().all(|station| station.valid())
            && self
                .worker_stations
                .iter()
                .map(|s| &s.id)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == self.worker_stations.len()
            && self.presentation_page <= 1024
            && self.presentation_content_page <= 4096
            && self
                .projection
                .iter()
                .chain(&self.scene_to_space)
                .chain(&self.head)
                .chain(self.agents.iter().flatten())
                .chain(self.controller.iter().flatten())
                .all(|v| v.is_finite())
    }
}

pub fn receive_frames() -> Receiver<Frame> {
    let (sender, receiver) = mpsc::sync_channel(2);
    let port = std::env::var("TOMORI_BRIDGE_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .unwrap_or(8765);
    let url = format!("http://127.0.0.1:{port}/spectator/frame");
    std::thread::Builder::new()
        .name("spectator-receive".into())
        .spawn(move || {
            let client = ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_millis(400))
                .timeout_read(Duration::from_millis(400))
                .build();
            loop {
                if let Ok(response) = client.get(&url).call()
                    && let Ok(frame) = response.into_json::<Frame>()
                    && frame.valid()
                    && matches!(
                        sender.try_send(frame),
                        Err(mpsc::TrySendError::Disconnected(_))
                    )
                {
                    break;
                }
                std::thread::sleep(VIDEO_INTERVAL);
            }
        })
        .expect("start spectator receiver");
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_selection_is_shared_and_legacy_frames_remain_minimal() {
        let legacy = serde_json::json!({
            "sequence": 1, "projection": ([1.0; 16].to_vec()), "scene_to_space": ([1.0; 16].to_vec()),
            "head": ([1.0; 16].to_vec()), "view_aspect": 1.0, "agents": vec![[0.0; 3]; AGENTS],
            "controller": null, "attracting": false, "caption": "", "recording_ms": null
        });
        let mut frame: Frame = serde_json::from_value(legacy).unwrap();
        assert_eq!(frame.environment, crate::environment::Environment::Minimal);
        frame.environment = crate::environment::Environment::Sky;
        let mut encoded = serde_json::to_value(&frame).unwrap();
        assert_eq!(encoded["environment"], "sky");
        let decoded: Frame = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(decoded.environment, frame.environment);
        assert!(decoded.valid());
        encoded["environment"] = "unknown".into();
        assert!(serde_json::from_value::<Frame>(encoded).is_err());
    }

    #[test]
    fn terminal_motion_and_128_stations_roundtrip_without_spectator_reanimation() {
        use crate::delegation::{Node, Scene, State, Workspace};
        let mut scene = Scene::new();
        let workspace = Workspace {
            nodes: (0..64)
                .map(|i| Node {
                    id: format!("old-{i:03}"),
                    parent_id: Some("root".into()),
                    title: format!("Task {i}"),
                    state: if i % 2 == 0 {
                        State::Completed
                    } else {
                        State::Error
                    },
                })
                .collect(),
            ..Default::default()
        };
        for _ in 0..120 {
            scene.update(Some("root"), &workspace, 0.05);
        }
        let mut frame: Frame = serde_json::from_value(serde_json::json!({
            "sequence": 1, "projection": ([1.0; 16].to_vec()), "scene_to_space": ([1.0; 16].to_vec()),
            "head": ([1.0; 16].to_vec()), "view_aspect": 1.0, "agents": vec![[0.0; 3]; AGENTS],
            "controller": null, "attracting": false, "caption": "Synthetic", "recording_ms": null
        })).unwrap();
        frame.workspace = workspace;
        frame.worker_cubes = scene.cubes().to_vec();
        frame.worker_stations = scene.stations().to_vec();
        assert!(frame.valid());
        assert_ne!(
            frame.worker_cubes[0].orientation,
            glam::Quat::IDENTITY.to_array()
        );
        let encoded = serde_json::to_value(&frame).unwrap();
        let decoded: Frame = serde_json::from_value(encoded.clone()).unwrap();
        assert!(decoded.valid());
        assert_eq!(
            serde_json::to_value(&decoded.worker_cubes).unwrap(),
            serde_json::to_value(scene.cubes()).unwrap()
        );
        let mut legacy = encoded;
        for cube in legacy["worker_cubes"].as_array_mut().unwrap() {
            cube.as_object_mut().unwrap().remove("orientation");
        }
        let decoded: Frame = serde_json::from_value(legacy).unwrap();
        assert!(decoded.valid());
        assert!(
            decoded
                .worker_cubes
                .iter()
                .all(|c| c.orientation == glam::Quat::IDENTITY.to_array())
        );
        frame.worker_cubes[0].orientation = [1.0; 4];
        assert!(!frame.valid());
        frame.worker_cubes.clear();
        frame.workspace.nodes = (0..64)
            .map(|i| Node {
                id: format!("new-{i:03}"),
                parent_id: Some("root".into()),
                state: State::Running,
                ..Default::default()
            })
            .collect();
        scene.update(Some("root"), &frame.workspace, 0.05);
        frame.worker_cubes = scene.cubes().to_vec();
        frame.worker_stations = scene.stations().to_vec();
        assert_eq!(frame.worker_stations.len(), 128);
        assert!(frame.valid());
        let mut extra = frame.worker_stations[0].clone();
        extra.id = "extra".into();
        frame.worker_stations.push(extra);
        assert!(!frame.valid());
        frame.worker_stations.pop();
        frame.worker_stations[1].id = frame.worker_stations[0].id.clone();
        assert!(!frame.valid());
    }

    #[test]
    fn validates_scene_and_rejects_nonfinite_or_incomplete_frames() {
        let mut frame = Frame {
            sequence: 1,
            environment: Default::default(),
            projection: [1.0; 16],
            scene_to_space: [1.0; 16],
            head: [1.0; 16],
            view_aspect: 1.0,
            agents: vec![[0.0; 3]; AGENTS],
            controller: None,
            controllers: [None, None],
            hands: [None, None],
            attracting: false,
            caption: "Hello".into(),
            recording_ms: None,
            interaction: Default::default(),
            worker_cubes: Vec::new(),
            worker_stations: Vec::new(),
            workspace: Default::default(),
            presentation_page: 0,
            presentation_content_page: 0,
        };
        assert!(frame.valid());
        assert!(
            serde_json::from_slice::<Frame>(&serde_json::to_vec(&frame).unwrap())
                .unwrap()
                .valid()
        );
        frame.agents[0][0] = f32::NAN;
        assert!(!frame.valid());
        frame.agents.pop();
        assert!(!frame.valid());
    }

    #[test]
    fn controller_poses_are_optional_for_older_senders_and_validated() {
        let frame = Frame {
            sequence: 1,
            environment: Default::default(),
            projection: [1.0; 16],
            scene_to_space: [1.0; 16],
            head: [1.0; 16],
            view_aspect: 1.0,
            agents: vec![[0.0; 3]; AGENTS],
            controller: None,
            controllers: [None, None],
            hands: [None, None],
            attracting: false,
            caption: String::new(),
            recording_ms: None,
            interaction: Default::default(),
            worker_cubes: Vec::new(),
            worker_stations: Vec::new(),
            workspace: Default::default(),
            presentation_page: 0,
            presentation_content_page: 0,
        };
        let mut old = serde_json::to_value(&frame).unwrap();
        old.as_object_mut().unwrap().remove("controllers");
        old.as_object_mut().unwrap().remove("hands");
        old.as_object_mut().unwrap().remove("interaction");
        for key in [
            "worker_cubes",
            "worker_stations",
            "workspace",
            "presentation_page",
            "presentation_content_page",
        ] {
            old.as_object_mut().unwrap().remove(key);
        }
        assert!(serde_json::from_value::<Frame>(old).unwrap().valid());
        let mut bad = frame.clone();
        bad.presentation_page = 3;
        bad.presentation_content_page = 17;
        let roundtrip: Frame = serde_json::from_slice(&serde_json::to_vec(&bad).unwrap()).unwrap();
        assert_eq!(
            (
                roundtrip.presentation_page,
                roundtrip.presentation_content_page
            ),
            (3, 17)
        );
        bad.presentation_page = 0;
        bad.presentation_content_page = 0;
        bad.interaction.level = 2.0;
        assert!(!bad.valid());
        bad.interaction = Default::default();
        bad.presentation_page = 1025;
        assert!(!bad.valid());
        bad.presentation_page = 0;
        bad.presentation_content_page = 4097;
        assert!(!bad.valid());
        bad.presentation_content_page = 0;
        bad.worker_cubes = vec![crate::delegation::Cube {
            position: [f32::NAN, 0.0, 0.0],
            scale: [0.02; 3],
            color: [0.5; 3],
            orientation: glam::Quat::IDENTITY.to_array(),
        }];
        assert!(!bad.valid());
        bad.worker_cubes.clear();
        bad.worker_stations.push(crate::delegation::Station {
            id: "ses_worker".into(),
            position: [0.0; 3],
            state: crate::delegation::State::Running,
            title: "Worker".into(),
            opacity: f32::NAN,
        });
        assert!(!bad.valid());
        bad.worker_stations.clear();
        bad.controllers[0] = Some(crate::controller::Pose {
            position: [0.0; 3],
            orientation: [0.0; 4],
        });
        assert!(!bad.valid());
        bad.controllers[0] = None;
        let mut hand = crate::hands::Hand {
            joints: [None; crate::hands::JOINT_COUNT],
        };
        hand.joints[1] = Some(crate::hands::Joint {
            position: [0.0; 3],
            radius: -1.0,
        });
        bad.hands[0] = Some(hand);
        assert!(!bad.valid());
    }
}
