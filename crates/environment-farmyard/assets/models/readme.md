# The Farmyard's Models

The models the farmyard loads at runtime, through `tomori-assets`, each named in code by an `AssetRef` with the SHA-256 of its bytes, so that a file changed without its digest is refused. Builds embed them, except on Android, where the APK carries this folder as `assets/farmyard/`.

- `chicken.glb`: the chicken and its cone, with its walk, confused and sprint animations, by Hinoki Maple Williams, under the [Creative Commons Attribution 4.0 International licence](https://creativecommons.org/licenses/by/4.0/) (CC BY 4.0); see [`LICENSE.md`](LICENSE.md), the credit and notice that go with it wherever it goes. Its digest is `chicken::DIGEST`, `eb491ea326546b7c60db46c88cff9d1abf544a41e01eda0ac09de823e2d6515b`.

The chicken is exported from its `.blend` sources in [`../source`](../source/readme.md), which says how. Exporting it again gives the same bytes; a change to it changes its digest, in `src/chicken.rs`, with it, and the test that checks the runtime's skinning against Blender's own, from `chicken-reference.json`, checks it.
