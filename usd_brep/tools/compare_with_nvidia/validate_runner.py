"""Run NVIDIA's BrepArray validator on one or more .usda files."""
import sys
def main():
    REPO = sys.argv[1]
    sys.path.insert(0, REPO + "/tools")
    from brep_validator import validate as v
    schema = REPO + "/source/schema/omniSolid/resources"
    ok_all = True
    for f in sys.argv[2:]:
        r = v.validate_file(f, omnisolid_plugin_path=schema, brep_only=True, workers=1)
        ok_all &= r.passed
        print(f"{'PASS' if r.passed else 'FAIL'}  {f.split('/')[-1]}  breps={r.brep_array_count} failures={r.failure_count}")
        for m in r.messages[:8]:
            print("      ", m[:220])
    sys.exit(0 if ok_all else 1)

if __name__ == '__main__':
    main()