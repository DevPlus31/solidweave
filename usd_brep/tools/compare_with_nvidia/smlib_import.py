"""Load .usda files into NVIDIA's SMLib kernel and report each Brep.

Checks what NVIDIA's Python validator cannot: that SMLib itself accepts the
geometry as a closed manifold solid (seams, closed edges, hole walls), and
what volume it computes.

    python smlib_import.py <usd-brep checkout> FILE.usda...

Needs a built checkout (prebuild.bat, then repo.bat build -r) and Python 3.12,
the version the bindings are built for. Exits 1 if any Brep fails.
"""
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

    ok_all = True
    for path in sys.argv[2:]:
        name = os.path.basename(path)
        try:
            breps = sm.usd.import_breps(path)
        except RuntimeError as e:
            print(f"FAIL  {name}: import failed: {e}")
            ok_all = False
            continue
        for i, b in enumerate(breps):
            manifold = b.is_manifold_solid()
            ok_all &= manifold
            print(
                f"{'PASS' if manifold else 'FAIL'}  {name} brep {i}: manifold={manifold} "
                f"volume={b.volume(relative_accuracy=1e-10):.9f} area={b.area(relative_accuracy=1e-10):.9f} "
                f"F{b.face_count()} E{b.edge_count()} V{b.vertex_count()}"
            )
    sys.exit(0 if ok_all else 1)


if __name__ == "__main__":
    main()
