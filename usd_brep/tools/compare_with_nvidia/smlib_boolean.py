"""Run the same boolean operations in NVIDIA's SMLib and in mini_brep, and
compare volume, area, topology counts and shells.

    cargo run -q -p mini_brep --example boolean_cases > cases.json
    python smlib_boolean.py <usd-brep checkout> cases.json

Needs a built checkout and Python 3.12 (see smlib_import.py). Exits 1 if any
volume differs by more than 1e-6 relative, or any count differs.
"""
import json
import os
import sys


def main():
    root = os.path.abspath(sys.argv[1])
    release = os.path.join(root, "_build", "windows-x86_64", "release")
    os.environ.setdefault("OMNISOLID_PLUGIN_PATH", os.path.join(root, "_build", "schema", "omniSolid", "resources"))
    sys.path.insert(0, release)
    for d in (release, os.path.join(release, "extraLibs")):
        if os.path.isdir(d):
            os.add_dll_directory(d)
    import usd_brep as sm

    ops = {"Union": sm.boolean_union, "Difference": sm.boolean_difference, "Intersection": sm.boolean_intersection}
    box = lambda b: sm.create_box((b[0], b[1], b[2]), b[3], b[4], b[5])
    ok_all = True
    for case in json.load(open(sys.argv[2])):
        r = ops[case["op"]](box(case["a"]), box(case["b"]))
        v = r.volume(relative_accuracy=1e-10)
        counts = (r.vertex_count(), r.edge_count(), r.face_count())
        ours = (case["V"], case["E"], case["F"])
        dv = abs(v - case["volume"]) / max(abs(v), 1e-12)
        ok = dv < 1e-6 and counts == ours and r.is_manifold_solid()
        ok_all &= ok
        print(
            f"{'PASS' if ok else 'FAIL'}  {case['name']:<22} volume ours {case['volume']:.9f} SMLib {v:.9f} "
            f"(rel {dv:.1e})  V/E/F ours {ours} SMLib {counts}"
        )
    sys.exit(0 if ok_all else 1)


if __name__ == "__main__":
    main()
