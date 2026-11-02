# Arrival

This describes how a participant gets from putting the headset on to being in a space: choosing a server, choosing or creating a space, and binding an existing agent session. Parts of it are long-term; the open questions at the end are not yet decided.

## Requirement

Arrival takes place in the 3D scene, not on a heads-up display. Servers, spaces and the action that creates a space are objects placed in the world. Text that labels them is anchored to those objects, not to the participant's view. On a headset, the participant chooses an object by pointing a controller at it and pressing the trigger. Pointing gives visible feedback, such as a ray and a highlight, before anything is chosen. Stick and button shortcuts may remain alongside pointing, but pointing must work for every arrival step.

Servers appear in the scene as well, including when only one server is found. Each server gets a visual treatment designed to look good, not a flat list item. A server whose protocol is incompatible is shown with its version but can't be chosen, as it is today. The USB entry is always offered, as it is today, so the scene is never empty; choosing it when no server is listening fails visibly. Connecting automatically, to the remembered server or to one given at startup, still works. The scene shows that server as chosen, and the participant can go back to the servers to choose another.

The agent swarm is present from the moment the app starts, through choosing a server and a space, and into the space. It moves continuously between states and never disappears or jumps. Before a server is chosen, the swarm is an amorphous cloud. Choosing a server materializes the swarm into its circular, eye-like shape. That shape is the same for every server. This is a change: today the swarm stays a cloud until a space is joined. In the lobby there is no agent, so the shape shows no agent state. Inside a space it shows the agent's state as it does today. The swarm's existing look, an amorphous cloud of cubes that forms into its circular, eye-like shape, is kept, and arrival builds on it rather than replacing it.

Choosing a space and creating one happen in one view. Once connected, the participant sees every space on the server alongside an object that creates a new one. When there are no spaces, the view shows the create object directly. There is no separate creation page, and no instruction to press a button to reach one.

Spaces are visually distinct from each other. Each space's object has its own appearance, at least a colour and a form, derived deterministically from the space's identifier. It therefore looks the same to every participant and on every visit, and different spaces very likely look different. It also shows the space's live status: its activity (idle, running, waiting or closed), how many participants it has, and its title. Later, the object should become a preview of the space itself, such as a small version of its environment or scene (see `scene-modules.md`). The deterministic appearance is the fallback for a space that has no preview.

Arrival assumes no environment theme. Server objects, space objects and the swarm do not borrow the look of any one environment, such as the moonlit shrine in `crates/environment-sky`. Arrival works and reads clearly with every environment the client offers. The intended direction, not yet a requirement, is that each space has its own environment suited to its work, that arrival has its own neutral setting, and that entering a space moves from that setting into the space's environment. Today the environment is a per-device choice and isn't part of space state (see `spaces.md`).

When creating a space bound to an existing agent session, the participant can tell the sessions apart without opening them. Each listed session shows, as far as its backend can report them:

- its title,
- its git branch, when its directory is a repository,
- when it was last active,
- a short excerpt, such as its first prompt or last message.

Sessions are listed with the most recently active first. A backend that cannot report a field omits it; it never makes one up. The server derives these facts from what the backend reports, consistent with `agent-backends.md`. Sessions are listed for the directory already chosen, so the directory isn't repeated on each one.

## Rationale

The user found the heads-up panel unintuitive for getting connected, and found the separate "no spaces, press Y" step to create a space a detour. They asked for servers and spaces to be shown in 3D, chosen by pointing with the controllers, with spaces visually distinct and eventually previewed. They asked that the swarm's cloud-to-eye formation be kept and carried through arrival, and that arrival not assume the shrine theme, because they intend to build different environments for different kinds of work. Picking up an existing session needs more context than its name. Stated by the user, speaking from the headset, in November 2026.

## Scope

The headset client's flow from launch to joining a space (`crates/client-quest`), the swarm's arrival behaviour, the lobby and session-listing messages in `crates/protocol`, and the session facts each backend reports for attaching.

## Validation

- Run the headset client with no server running, with one server and with several. With no server, the USB entry is still shown, and choosing it fails visibly. With a remembered server, it is connected automatically and shown as chosen. Confirm that servers appear as objects in the world, that labels stay with their objects when the head turns, and that a server can be chosen by pointing and pulling the trigger.
- Confirm that the swarm is visible throughout launch, server choice, space choice and joining, and that it changes shape continuously. It starts as a cloud, forms its shape when a server is chosen, and shows the agent's state only inside a space.
- Connect to a server with no spaces and confirm that the create object is shown directly. Connect to one with spaces and confirm that the create object sits among them in the same view.
- Test that a space's colour and form are a function of its identifier alone, and that the same space looks the same after reconnecting and to a second participant. Confirm that a change in a space's activity or participant count shows on its object.
- Run arrival with each environment the client offers, currently Sky and Minimal. It works and reads the same with both, and its objects use none of an environment's meshes or materials.
- Test the session listing for each backend that supports attaching: sessions are ordered by last activity, the reported fields match the agent's own session records (for Claude Code, its transcripts under `~/.claude/projects`; for OpenCode, its session API), and fields the backend can't report are absent rather than invented.

Open questions, not yet decided:

- The visual strategy for servers, and how the swarm materializes from a server object.
- What the neutral arrival setting looks like, whether the existing Minimal environment is it, and how the transition into a space's environment looks.
- Whether the environment becomes part of a space's shared state, how a space chooses it, and where that choice is stored.
- How space previews are rendered and what they cost on the headset. `scene-modules.md` replicates a space's scene only to the clients in it, so a lobby preview needs either a summary the server makes for the lobby or replication to clients outside the space.
- How creation choices other than the session, such as the backend and directory, are presented in 3D.
- Whether hand-tracking selection, such as a pinch, is supported alongside controllers.
