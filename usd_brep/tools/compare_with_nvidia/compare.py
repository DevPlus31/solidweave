"""Compare Rust and Python validator results per file."""
import json, sys
py = json.load(open(sys.argv[1])); rs = json.load(open(sys.argv[2]))
files = sorted(k for k in py if py[k]["breps"] > 0)
agree = 0; detail = []
both_code = 0; tested = 0
for f in files:
    p, r = set(py[f]["codes"]), set(rs.get(f, {}).get("codes", []))
    same_verdict = (not p) == (not r)
    agree += same_verdict
    name = f.split("/")[-1]
    if name.startswith("Test_BA_"):
        code = "BA." + name.split("_")[2]
        tested += 1
        both_code += (code in r)
    if not same_verdict or (p != r and "-v" in sys.argv):
        detail.append(f"{'MISMATCH' if not same_verdict else 'codes   '} {name}\n     python: {sorted(p)}\n     rust:   {sorted(r)}")
print("\n".join(detail))
print(f"\nverdict agreement: {agree}/{len(files)} files")
print(f"Test_BA_xxx files where Rust reports the targeted code: {both_code}/{tested}")
py_pass = [f for f in files if not py[f]['codes']]
print(f"files Python passes: {len(py_pass)}; Rust also passes: {sum(1 for f in py_pass if not rs[f]['codes'])}")
