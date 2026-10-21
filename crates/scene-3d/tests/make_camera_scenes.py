# Builds test scenes with cameras and records where Blender projects points.
# Run: blender --background --factory-startup --python make_camera_scene.py -- <out_dir>
import json
import math
import sys

import bpy
from bpy_extras.object_utils import world_to_camera_view
from mathutils import Vector

out_dir = sys.argv[sys.argv.index("--") + 1]


def build(name, ortho, fit, res, percent, shift, rotation_mode, parent):
    bpy.ops.wm.read_factory_settings(use_empty=True)
    scene = bpy.context.scene
    bpy.ops.mesh.primitive_cube_add(location=(1.0, 2.0, 0.5), rotation=(0.0, 0.0, math.radians(20)))
    cam_data = bpy.data.cameras.new("Cam")
    cam = bpy.data.objects.new("Cam", cam_data)
    scene.collection.objects.link(cam)
    cam_data.lens = 35.0
    cam_data.sensor_width = 36.0
    cam_data.sensor_height = 24.0
    cam_data.sensor_fit = fit
    cam_data.shift_x, cam_data.shift_y = shift
    cam_data.clip_start, cam_data.clip_end = 0.2, 250.0
    if ortho:
        cam_data.type = "ORTHO"
        cam_data.ortho_scale = 9.0
    cam.rotation_mode = rotation_mode
    if parent:
        empty = bpy.data.objects.new("Rig", None)
        scene.collection.objects.link(empty)
        empty.location = (2.0, -3.0, 1.0)
        empty.rotation_euler = (0.0, 0.0, math.radians(25))
        empty.scale = (1.5, 1.5, 1.5)
        cam.parent = empty
        cam.location = (3.0, -6.0, 2.0)
    else:
        cam.location = (7.0, -8.0, 5.0)
    if rotation_mode == "QUATERNION":
        cam.rotation_quaternion = (0.78, 0.48, 0.21, 0.34)
    else:
        cam.rotation_euler = (math.radians(62), math.radians(4), math.radians(38))
    scene.camera = cam
    scene.render.resolution_x, scene.render.resolution_y = res
    scene.render.resolution_percentage = percent
    bpy.context.view_layer.update()
    points = [(0.0, 0.0, 0.0), (1.0, 2.0, 0.5), (-2.0, 3.0, 1.5), (3.0, 1.0, -1.0), (0.5, 4.0, 2.0)]
    expected = []
    for p in points:
        v = world_to_camera_view(scene, cam, Vector(p))
        expected.append({"point": p, "frame": [v.x, v.y], "depth": v.z})
    # Vanishing points of the world axes: a point very far along each axis, in front of the camera.
    vanishing = []
    loc = cam.matrix_world.translation
    forward = cam.matrix_world.to_quaternion() @ Vector((0.0, 0.0, -1.0))
    for axis in (Vector((1, 0, 0)), Vector((0, 1, 0)), Vector((0, 0, 1))):
        d = axis if axis.dot(forward) >= 0 else -axis
        if ortho or abs(axis.dot(forward)) < 1e-3:
            vanishing.append(None)
            continue
        v = world_to_camera_view(scene, cam, loc + d * 1e6)
        vanishing.append([v.x, v.y])
    w = round(res[0] * percent / 100)
    h = round(res[1] * percent / 100)
    path = f"{out_dir}/{name}.blend"
    bpy.ops.wm.save_as_mainfile(filepath=path)
    loc = cam.matrix_world.translation
    return {"file": path, "width": w, "height": h, "ortho": ortho, "location": list(loc), "vanishing": vanishing, "clip": [0.2, 250.0], "points": expected}


cases = [
    build("persp_parented_xzy", False, "AUTO", (1600, 1200), 50, (0.1, -0.05), "XZY", True),
    build("persp_quat_vertical_fit", False, "VERTICAL", (1080, 1920), 100, (0.0, 0.12), "QUATERNION", False),
    build("ortho_xyz", True, "AUTO", (1920, 1080), 100, (0.05, 0.0), "XYZ", False),
]
with open(f"{out_dir}/expected.json", "w") as f:
    json.dump(cases, f, indent=1)
print("WROTE", len(cases))
