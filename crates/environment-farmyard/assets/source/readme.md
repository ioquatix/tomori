# The Chicken's Sources

The chicken as its author, Hinoki Maple Williams, made it in Blender, under the [Creative Commons Attribution 4.0 International licence](https://creativecommons.org/licenses/by/4.0/) (CC BY 4.0), as [`../models/LICENSE.md`](../models/LICENSE.md) says:

- `chickenwalkcycleanim.blend`: the chicken, its cone and its walk;
- `chickenDLconfused.blend`: its confused head tilt;
- `chickensprintcycleanim.blend`: its startled sprint.

Building Tomori never needs Blender: the exported `../models/chicken.glb` is committed, and the `.blend` files are left out of the crate's package. To export it again, with Blender 5.2.3, from this folder:

```sh
blender -b --disable-autoexec --factory-startup "$PWD/chickenwalkcycleanim.blend" \
    --python "$PWD/export.py" -- \
    "$PWD/../models/chicken.glb" "$PWD/chicken-reference.json" \
    walk confused="$PWD/chickenDLconfused.blend" sprint="$PWD/chickensprintcycleanim.blend"
```

`--disable-autoexec` runs no script the `.blend` files hold, and `export.py` saves nothing back to them. It names the three actions `walk`, `confused` and `sprint`, splits the cone into a mesh of its own, `cone`, skinned to the same armature as the body, `chicken`, scales the textures to 512 × 512, and exports a `.glb` with every animation as an animation of its own. Beside it, `chicken-reference.json` keeps every seventh vertex of each mesh as Blender deforms it at a few frames of each animation, which a test checks the runtime's skinning against. The export is deterministic: running it again gives the same bytes, and the same digest.
