# The Farmyard's Chickens

The farmyard's chickens are split as a model, a view and a controller, across its two crates; Tomori's `design/farmyard.md` is the full account.

## The parts

- `chicken`: the asset, `assets/models/chicken.glb`, named by `chicken::DIGEST`; `Chicken::shared()` loads it once from `tomori_assets::Library::shared()`, measures how it stands and how far its walk and sprint carry it, and `Chicken::graph(pace)` gives its animator graph. `bundle()` embeds it everywhere but Android, where the APK carries it as `farmyard/chicken.glb`.
- `behaviour`: a `Behaviour` is what a chicken does (`Doing`) and since when, on the farmyard's clock, in milliseconds; `place`, `walk` and `ends` say where that puts it, the same on the server and every client.
- `flock`: the `Flock` of a space, seeded by the space's identifier: `update(now)` moves it on, `ask(participant, Intent, now)` takes a `notice` or `away`, `describe(now)` says what the agent is told. `State`, `Event` and `Intent` are its JSON on the environment state channel.
- `side`: `FarmyardSide`, the server side, which shares the state, announces events and describes the chickens.
- `controller`: `ChickenController` drives one chicken's animator and instance from its behaviours, and returns the animator's events (`STEP`, `CLUCK`, `SQUAWK`).
- `cluck`: the clucks, synthesized once (`rendered()`), which clients play as ambient sounds.
- In `tomori-environment-farmyard-client`, `client::FarmyardClient` is the client side, and the scenery draws what it shows.

## Changing them

A change to what the server and clients exchange, the JSON of `State`, `Event` or `Intent`, or to what both compute, such as `Behaviour::place`, the layout or the chicken's asset, raises the farmyard's version, `VERSION` in this crate, unless an older client can ignore it. To replace the chicken, export it again as `assets/source/readme.md` says, and update `DIGEST` and the tests that keep its stride and sprint.
