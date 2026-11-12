# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

from __future__ import annotations

import hashlib
import os
import stat
import tempfile
import unittest
from pathlib import Path

from npm_patch import apply_patch

# A real patch file from pnpm 11 (patches/type@2.7.3.patch).
PNPM_11_PATCH = """\
diff --git a/NEWFILE.txt b/NEWFILE.txt
new file mode 100644
index 0000000000000000000000000000000000000000..fa49b077972391ad58037050f2a75f74e3671e92
--- /dev/null
+++ b/NEWFILE.txt
@@ -0,0 +1 @@
+new file
diff --git a/README.md b/README.md
deleted file mode 100644
index 65eab8809ce975ee727fba59be4b5d3ab063eaf3..0000000000000000000000000000000000000000
diff --git a/value/is.js b/value/is.js
index 648aa9c7b0146e2e369fec1db2f103c65a9a36b2..3d1500a53b908c227d611ddbfc6250caf50bc6a6 100644
--- a/value/is.js
+++ b/value/is.js
@@ -4,3 +4,4 @@
 var _undefined = void 0;

 module.exports = function (value) { return value !== _undefined && value !== null; };
+module.exports.PATCHED = true;
"""

IS_JS = (
    '"use strict";\n'
    "\n"
    "var _undefined = void 0;\n"
    "\n"
    "module.exports = function (value) { return value !== _undefined && value !== null; };\n"
)


class NpmPatchTest(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = Path(tmp.name)
        self.pkg = self.tmp / "pkg"
        self.pkg.mkdir()

    def write(self, rel: str, data: bytes | str) -> None:
        path = self.pkg / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data if isinstance(data, bytes) else data.encode())

    def read(self, rel: str) -> bytes:
        return (self.pkg / rel).read_bytes()

    def patch(self, text: bytes | str, expected_hash: str | None = None) -> None:
        path = self.tmp / "p.patch"
        path.write_bytes(text if isinstance(text, bytes) else text.encode())
        apply_patch(path, self.pkg, expected_hash)

    def fails(self, text: bytes | str, *needles: str) -> str:
        with self.assertRaises(ValueError) as cm:
            self.patch(text)
        message = str(cm.exception)
        self.assertIn("p.patch", message)
        for needle in needles:
            self.assertIn(needle, message)
        return message

    def test_pnpm_11_fixture(self) -> None:
        self.write("README.md", "# readme\n")
        self.write("value/is.js", IS_JS)
        self.patch(PNPM_11_PATCH)
        self.assertEqual(self.read("NEWFILE.txt"), b"new file\n")
        self.assertFalse((self.pkg / "README.md").exists())
        self.assertEqual(self.read("value/is.js"), (IS_JS + "module.exports.PATCHED = true;\n").encode())

    def test_multiple_hunks(self) -> None:
        self.write("f.txt", "".join(f"l{i}\n" for i in range(1, 21)))
        self.patch(
            "--- a/f.txt\n+++ b/f.txt\n"
            "@@ -2,3 +2,2 @@ fn one\n l2\n-l3\n l4\n"
            "@@ -18,3 +17,4 @@\n l18\n l19\n+extra\n l20\n"
        )
        lines = self.read("f.txt").decode().split()
        self.assertNotIn("l3", lines)
        self.assertEqual(lines[-3:], ["l19", "extra", "l20"])

    def test_new_executable_file(self) -> None:
        self.patch("diff --git a/bin/x b/bin/x\nnew file mode 100755\n--- /dev/null\n+++ b/bin/x\n@@ -0,0 +1,2 @@\n+#!/bin/sh\n+echo\n")
        self.assertEqual(self.read("bin/x"), b"#!/bin/sh\necho\n")
        self.assertEqual(stat.S_IMODE((self.pkg / "bin/x").stat().st_mode), 0o755)

    def test_new_empty_file(self) -> None:
        self.patch("diff --git a/e b/e\nnew file mode 100644\n")
        self.assertEqual(self.read("e"), b"")

    def test_new_file_already_exists(self) -> None:
        self.write("e", "x")
        self.fails("diff --git a/e b/e\nnew file mode 100644\n", "e", "already exists")

    def test_delete_with_hunks(self) -> None:
        self.write("d.txt", "a\nb\n")
        self.patch("diff --git a/d.txt b/d.txt\ndeleted file mode 100644\n--- a/d.txt\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-a\n-b\n")
        self.assertFalse((self.pkg / "d.txt").exists())

    def test_delete_with_mismatched_hunks(self) -> None:
        self.write("d.txt", "a\nc\n")
        self.fails(
            "diff --git a/d.txt b/d.txt\ndeleted file mode 100644\n--- a/d.txt\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-a\n-b\n",
            "d.txt",
            "@@ -1,2 +0,0 @@",
        )
        self.assertTrue((self.pkg / "d.txt").exists())

    def test_delete_with_partial_hunks(self) -> None:
        self.write("d.txt", "a\nb\nc\n")
        self.fails(
            "diff --git a/d.txt b/d.txt\ndeleted file mode 100644\n--- a/d.txt\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-a\n-b\n",
            "d.txt",
            "still has content",
        )

    def test_mode_change(self) -> None:
        self.write("run.sh", "x\n")
        os.chmod(self.pkg / "run.sh", 0o644)
        self.patch("diff --git a/run.sh b/run.sh\nold mode 100644\nnew mode 100755\n")
        self.assertEqual(stat.S_IMODE((self.pkg / "run.sh").stat().st_mode), 0o755)
        self.patch("diff --git a/run.sh b/run.sh\nold mode 100755\nnew mode 100644\n")
        self.assertEqual(stat.S_IMODE((self.pkg / "run.sh").stat().st_mode), 0o644)
        self.assertEqual(self.read("run.sh"), b"x\n")

    def test_rename_without_hunks(self) -> None:
        self.write("old/a.js", "x\n")
        self.patch("diff --git a/old/a.js b/new/b.js\nsimilarity index 100%\nrename from old/a.js\nrename to new/b.js\n")
        self.assertFalse((self.pkg / "old/a.js").exists())
        self.assertEqual(self.read("new/b.js"), b"x\n")

    def test_rename_with_hunks(self) -> None:
        self.write("a.js", "one\ntwo\n")
        self.patch(
            "diff --git a/a.js b/b.js\nsimilarity index 80%\nrename from a.js\nrename to b.js\n"
            "--- a/a.js\n+++ b/b.js\n@@ -1,2 +1,2 @@\n one\n-two\n+three\n"
        )
        self.assertFalse((self.pkg / "a.js").exists())
        self.assertEqual(self.read("b.js"), b"one\nthree\n")

    def test_copy_rejected(self) -> None:
        self.write("a", "x\n")
        self.fails("diff --git a/a b/b\nsimilarity index 100%\ncopy from a\ncopy to b\n", "copies")

    def test_no_newline_markers(self) -> None:
        self.write("a", "one\ntwo")
        self.patch("--- a/a\n+++ b/a\n@@ -1,2 +1,3 @@\n one\n-two\n\\ No newline at end of file\n+two\n+three\n\\ No newline at end of file\n")
        self.assertEqual(self.read("a"), b"one\ntwo\nthree")
        self.write("b", "one\ntwo\n")
        self.patch("--- a/b\n+++ b/b\n@@ -1,2 +1,2 @@\n one\n-two\n+two\n\\ No newline at end of file\n")
        self.assertEqual(self.read("b"), b"one\ntwo")
        # A context line that lacks its newline on both sides.
        self.write("c", "x\ny")
        self.patch("--- a/c\n+++ b/c\n@@ -1,2 +1,3 @@\n+w\n x\n y\n\\ No newline at end of file\n")
        self.assertEqual(self.read("c"), b"w\nx\ny")

    def test_no_newline_mismatch(self) -> None:
        self.write("a", "one\ntwo\n")
        self.fails("--- a/a\n+++ b/a\n@@ -1,2 +1,2 @@\n one\n-two\n\\ No newline at end of file\n+2\n")

    def test_quoted_paths(self) -> None:
        name = "with spaceé.js"
        self.write(name, "a\n")
        self.patch(
            'diff --git "a/with space\\303\\251.js" "b/with space\\303\\251.js"\n'
            '--- "a/with space\\303\\251.js"\n+++ "b/with space\\303\\251.js"\n@@ -1 +1 @@\n-a\n+b\n'
        )
        self.assertEqual(self.read(name), b"b\n")
        self.write("t\tab\"q.js", "a\n")
        self.patch('--- "a/t\\tab\\"q.js"\n+++ "b/t\\tab\\"q.js"\n@@ -1 +1 @@\n-a\n+c\n')
        self.assertEqual(self.read('t\tab"q.js'), b"c\n")

    def test_unquoted_path_with_space_and_no_hunks(self) -> None:
        self.write("a b.txt", "x")
        self.patch("diff --git a/a b.txt b/a b.txt\ndeleted file mode 100644\n")
        self.assertFalse((self.pkg / "a b.txt").exists())

    def test_hunk_header_without_counts_and_function_text(self) -> None:
        self.write("a", "x\ny\n")
        self.patch("--- a/a\n+++ b/a\n@@ -1 +1 @@ function foo() {\n-x\n+z\n")
        self.assertEqual(self.read("a"), b"z\ny\n")

    def test_without_diff_git_line(self) -> None:
        self.write("a", "1\n")
        self.write("b", "2\n")
        self.patch("--- a/a\n+++ b/a\n@@ -1 +1 @@\n-1\n+one\n--- a/b\n+++ b/b\n@@ -1 +1 @@\n-2\n+two\n")
        self.assertEqual(self.read("a"), b"one\n")
        self.assertEqual(self.read("b"), b"two\n")

    def test_context_mismatch(self) -> None:
        self.write("a.js", "one\ntwo\nthree\n")
        message = self.fails(
            "diff --git a/a.js b/a.js\n--- a/a.js\n+++ b/a.js\n@@ -1,3 +1,3 @@\n one\n-TWO\n+2\n three\n",
            "a.js",
            "@@ -1,3 +1,3 @@",
        )
        self.assertIn("do not match", message)
        self.assertEqual(self.read("a.js"), b"one\ntwo\nthree\n")

    def test_offset_match(self) -> None:
        self.write("f", "".join(f"l{i}\n" for i in range(1, 31)))
        # The patch was made against a file with 5 fewer lines before the hunks.
        self.patch(
            "--- a/f\n+++ b/f\n"
            "@@ -5,3 +5,3 @@\n l9\n-l10\n+ten\n l11\n"
            "@@ -20,3 +20,3 @@\n l24\n-l25\n+twentyfive\n l26\n"
        )
        data = self.read("f").decode()
        self.assertIn("l9\nten\nl11\n", data)
        self.assertIn("l24\ntwentyfive\nl26\n", data)
        self.assertNotIn("l10\n", data)

    def test_offset_backwards(self) -> None:
        self.write("f", "a\nb\nc\n")
        self.patch("--- a/f\n+++ b/f\n@@ -40,2 +40,2 @@\n b\n-c\n+C\n")
        self.assertEqual(self.read("f"), b"a\nb\nC\n")

    def test_overlapping_hunks_rejected(self) -> None:
        self.write("f", "a\nb\nc\n")
        # The second hunk can only match where the first one already matched.
        self.fails("--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n a\n-b\n+B\n@@ -1,2 +1,2 @@\n a\n-b\n+X\n", "f")

    def test_hash_match_and_mismatch(self) -> None:
        self.write("a", "x\n")
        text = "--- a/a\n+++ b/a\n@@ -1 +1 @@\n-x\n+y\n"
        digest = hashlib.sha256(text.encode()).hexdigest()
        self.patch(text, digest)
        self.assertEqual(self.read("a"), b"y\n")

        self.write("a", "x\n")
        with self.assertRaises(ValueError) as cm:
            self.patch(text, "0" * 64)
        message = str(cm.exception)
        self.assertIn("p.patch", message)
        self.assertIn("0" * 64, message)
        self.assertIn(digest, message)
        self.assertIn("pnpm install", message)
        self.assertEqual(self.read("a"), b"x\n")

    def test_old_hash_format_is_not_checked(self) -> None:
        self.write("a", "x\n")
        self.patch("--- a/a\n+++ b/a\n@@ -1 +1 @@\n-x\n+y\n", "5jwqfkxh7zd2mhkx2lqr6fm5zq")
        self.assertEqual(self.read("a"), b"y\n")

    def test_dotdot_rejected(self) -> None:
        self.write("a", "x\n")
        (self.tmp / "outside").write_text("x\n")
        self.fails("--- a/../outside\n+++ b/../outside\n@@ -1 +1 @@\n-x\n+y\n", "outside", "..")
        self.fails("--- a/x/../../outside\n+++ b/x/../../outside\n@@ -1 +1 @@\n-x\n+y\n", "..")
        self.fails("diff --git a/../new b/../new\nnew file mode 100644\n", "..")
        self.assertEqual((self.tmp / "outside").read_text(), "x\n")
        self.assertFalse((self.tmp / "new").exists())

    def test_absolute_path_rejected(self) -> None:
        target = self.tmp / "abs"
        target.write_text("x\n")
        self.fails(f"--- {target}\n+++ {target}\n@@ -1 +1 @@\n-x\n+y\n", "absolute")
        self.fails(f"--- a/{target}\n+++ b/{target}\n@@ -1 +1 @@\n-x\n+y\n", "absolute")
        self.fails("--- a//etc/passwd\n+++ b//etc/passwd\n@@ -1 +1 @@\n-x\n+y\n", "absolute")
        self.assertEqual(target.read_text(), "x\n")

    def test_symlink_escape_rejected(self) -> None:
        outside = self.tmp / "outside"
        outside.mkdir()
        (outside / "f").write_text("x\n")
        os.symlink(outside, self.pkg / "link")
        self.fails("--- a/link/f\n+++ b/link/f\n@@ -1 +1 @@\n-x\n+y\n", "link/f", "outside")
        self.fails("diff --git a/link/n b/link/n\nnew file mode 100644\n", "outside")
        self.assertEqual((outside / "f").read_text(), "x\n")
        self.assertFalse((outside / "n").exists())

    def test_crlf(self) -> None:
        self.write("w.txt", b"one\r\ntwo\r\nthree\r\n")
        self.patch(b"--- a/w.txt\r\n+++ b/w.txt\r\n@@ -1,3 +1,3 @@\r\n one\r\n-two\r\n+2\r\n three\r\n")
        self.assertEqual(self.read("w.txt"), b"one\r\n2\r\nthree\r\n")

    def test_crlf_file_with_lf_patch_does_not_match(self) -> None:
        self.write("w.txt", b"one\r\ntwo\r\n")
        self.fails("--- a/w.txt\n+++ b/w.txt\n@@ -1,2 +1,2 @@\n one\n-two\n+2\n", "w.txt")

    def test_binary_rejected(self) -> None:
        self.write("img.png", b"\x89PNG")
        self.fails(
            "diff --git a/img.png b/img.png\nindex 1234..5678 100644\nGIT binary patch\nliteral 4\nLcmZ?\n\n",
            "img.png",
            "binary",
        )
        self.fails("diff --git a/img.png b/img.png\nindex 1234..5678 100644\nBinary files a/img.png and b/img.png differ\n", "img.png", "binary")

    def test_symlink_mode_rejected(self) -> None:
        self.fails("diff --git a/l b/l\nnew file mode 120000\n--- /dev/null\n+++ b/l\n@@ -0,0 +1 @@\n+target\n\\ No newline at end of file\n", "120000")

    def test_missing_file(self) -> None:
        self.fails("--- a/nope\n+++ b/nope\n@@ -1 +1 @@\n-x\n+y\n", "nope", "does not exist")

    def test_malformed(self) -> None:
        self.write("a", "x\n")
        self.fails("--- a/a\n+++ b/a\n@@ nonsense @@\n", "a", "@@ nonsense @@")
        self.fails("--- a/a\n+++ b/a\n@@ -1,3 +1,3 @@\n-x\n", "a", "truncated")
        self.fails("--- a/a\n+++ b/a\n@@ -1 +1 @@\n?x\n", "a", "@@ -1 +1 @@")


if __name__ == "__main__":
    unittest.main()
