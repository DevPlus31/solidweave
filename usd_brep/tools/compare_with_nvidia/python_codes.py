"""Run NVIDIA's validator on many files; print JSON {file: [failure codes]}."""
import json, sys, os
def main():
    repo = sys.argv[1]
    sys.path.insert(0, repo + "/tools")
    os.environ["OMNISOLID_PLUGIN_PATH"] = repo + "/source/schema/omniSolid/resources"
    from brep_validator import validate as v
    out = {}
    for f in sys.argv[2:]:
        r = v.validate_file(f, brep_only=True, workers=1)
        out[os.path.relpath(f, repo)] = {"breps": r.brep_array_count, "codes": sorted(r.issue_summary)}
    json.dump(out, sys.stdout, indent=1, sort_keys=True)
if __name__ == "__main__":
    main()
