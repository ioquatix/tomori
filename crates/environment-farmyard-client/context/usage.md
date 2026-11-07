# Using the Farmyard's Client Half

`register(sceneries)` registers the farmyard with a client's `tomori_render::scenery::Sceneries`: its scenery, at `ID` and `VERSION`, the identity in `tomori-environment-farmyard`; its teleport profile (`locomotion()`); its client side, which shows the chickens; and the chicken's licence notice, `LICENSE`, under `LICENSE_TITLE`, which `tomori --licenses` shows. `register_with(sceneries, coop)` does so with another coop, such as one modelled in Blender (see `tomori-environment-farmyard`'s `context/coop.md`).

- `scenery::Farmyard` is the `Scenery`: the evening sky, the yard, the coop and, through `Scenery::animate`, the chickens, drawn with `tomori-assets`' renderer and lit by `scenery::lighting()`.
- `client::FarmyardClient` is the `ClientSide` of one space: it reads the flock's state and events from the farmyard's server side, keeps the farmyard's clock from them, drives a `ChickenController` for each chicken, picks the chicken the ray meets, asks the server side to notice it and to stop, and returns the clucks to play and the chickens to draw (`client::Drawn`).

A headset build that registers it should carry `tomori-environment-farmyard`'s `assets/models` in its APK as `farmyard/`, as Tomori's own does in `android/assets`, licence notice and all; see `tomori-client-quest`'s `context/android.md`. The chicken is by Hinoki Maple Williams, under CC BY 4.0, which asks that whoever shares it credits her and links the licence, as that notice does.
