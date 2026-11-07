# Export the chicken to glTF 2.0 binary for Tomori's farmyard, with Blender in background mode:
#
#     blender -b --disable-autoexec --factory-startup WALK.blend --python export.py -- \
#         OUTPUT.glb REFERENCE.json walk confused=CONFUSED.blend sprint=SPRINT.blend
#
# with absolute paths. It reads the .blend files as data only (--disable-autoexec runs nothing in them) and saves
# nothing back to any of them. The first file is the one Blender opened; its "ArmatureAction" becomes the first
# animation, named as given, and each other file's "ArmatureAction" is appended and named as given. The cone is split
# from the body by its material into a mesh of its own, "cone", skinned to the same armature as the body, "chicken",
# so that a chicken can be drawn without it. Textures are scaled to 512 x 512. Alongside, REFERENCE.json holds a few
# vertices of each mesh as Blender deforms them at a few frames of each animation, in glTF's axes, to check the
# runtime's skinning against.
import json
import sys

import bpy

argv = sys.argv[sys.argv.index("--") + 1:]
output, reference, first = argv[0], argv[1], argv[2]
others = [arg.split("=", 1) for arg in argv[3:]]
# The frames of each animation the reference holds, as Blender numbers them.
FRAMES = {"walk": [37, 43, 49, 55, 61, 67], "confused": [1, 24, 45, 70, 90], "sprint": [1, 12, 24, 40, 72]}
# How many vertices of each mesh the reference holds: every so many, in Blender's order.
EVERY = 7

armature = bpy.data.objects["Armature"]
body = bpy.data.objects["Cube.001"]
walk = armature.animation_data.action
walk.name = first
actions = [walk]
for name, path in others:
    with bpy.data.libraries.load(path, link=False) as (source, target):
        if "ArmatureAction" not in source.actions:
            raise SystemExit(f"{path} has no ArmatureAction")
        target.actions = ["ArmatureAction"]
    action = target.actions[0]
    action.name = name
    action.use_fake_user = True
    actions.append(action)

for image in ("chicken base", "chicken cone"):
    bpy.data.images[image].scale(512, 512)

# The cone, split from the body by its material.
for obj in bpy.context.view_layer.objects:
    obj.select_set(obj == body)
bpy.context.view_layer.objects.active = body
bpy.ops.object.mode_set(mode="EDIT")
bpy.ops.mesh.select_all(action="SELECT")
bpy.ops.mesh.separate(type="MATERIAL")
bpy.ops.object.mode_set(mode="OBJECT")
parts = [obj for obj in bpy.data.objects if obj.type == "MESH" and obj.parent == armature]


cones = [obj for obj in parts if any(
    node.type == "TEX_IMAGE" and node.image and node.image.name == "chicken cone"
    for node in obj.data.materials[obj.data.polygons[0].material_index].node_tree.nodes)]
if len(parts) != 2 or len(cones) != 1:
    raise SystemExit(f"expected a body and a cone, found {[obj.name for obj in parts]}")
cone = cones[0]
chicken = next(obj for obj in parts if obj != cone)
for obj, name in ((chicken, "chicken"), (cone, "cone")):
    obj.name = name
    obj.data.name = name
    # Only the material its faces use.
    used = {polygon.material_index for polygon in obj.data.polygons}
    for index in reversed(range(len(obj.data.materials))):
        if index not in used:
            obj.data.materials.pop(index=index)

# Blender's own deformation, before the animations become NLA tracks for the export.
scene = bpy.context.scene
dumped = {}
for action in actions:
    armature.animation_data.action = action
    if hasattr(armature.animation_data, "action_slot") and action.slots:
        armature.animation_data.action_slot = action.slots[0]
    frames = {}
    for frame in FRAMES[action.name]:
        scene.frame_set(frame)
        depsgraph = bpy.context.evaluated_depsgraph_get()
        meshes = {}
        for obj in (chicken, cone):
            evaluated = obj.evaluated_get(depsgraph)
            mesh = evaluated.to_mesh()
            world = evaluated.matrix_world
            meshes[obj.name] = [
                [round(c * 10000) for c in ((world @ v.co).x, (world @ v.co).z, -(world @ v.co).y)]
                for v in list(mesh.vertices)[::EVERY]
            ]
            evaluated.to_mesh_clear()
        frames[str(frame)] = meshes
    dumped[action.name] = frames
with open(reference, "w") as file:
    json.dump({"fps": scene.render.fps, "scale": 10000, "every": EVERY, "animations": dumped}, file, sort_keys=True, separators=(",", ":"))

# Each animation becomes an NLA strip of the armature's, which the exporter writes as an animation of its own.
data = armature.animation_data
data.action = None
for action in actions:
    track = data.nla_tracks.new()
    track.name = action.name
    strip = track.strips.new(action.name, int(action.frame_range[0]), action)
    strip.name = action.name
    if hasattr(strip, "action_slot") and action.slots:
        strip.action_slot = action.slots[0]
    track.mute = False

for obj in bpy.context.view_layer.objects:
    obj.select_set(obj in (chicken, cone, armature))
bpy.context.view_layer.objects.active = armature
bpy.ops.export_scene.gltf(
    filepath=output,
    export_format="GLB",
    use_selection=True,
    export_yup=True,
    export_apply=False,
    export_texcoords=True,
    export_normals=True,
    export_materials="EXPORT",
    export_image_format="AUTO",
    export_skins=True,
    export_all_influences=True,
    export_animations=True,
    export_animation_mode="NLA_TRACKS",
    export_force_sampling=True,
    export_anim_slide_to_zero=False,
    export_morph=False,
    export_cameras=False,
    export_lights=False,
    export_extras=False,
)
print("EXPORTED", output, [(action.name, tuple(action.frame_range)) for action in actions])
