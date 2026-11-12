import json
import sys

# A module that Vite knows nothing about: buck2 generates it from release.json.
with open(sys.argv[1]) as f:
    release = json.load(f)
with open(sys.argv[2], "w") as f:
    f.write("export const release = %s as const;\n" % json.dumps(release))
