"""Exercise the executable boundary used by native Python fixtures."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class PythonFixture(unittest.TestCase):
    def test_launcher_preserves_loader_environment_and_arguments(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "fixture's source.py"
            source.write_text('import json,os,sys; print(json.dumps([os.getenv("DYLD_LIBRARY_PATH"),sys.argv[1:]]))\n')
            launcher = root / 'launcher'
            subprocess.run([sys.executable, str(ROOT / 'scripts/fixtures/python-launcher.py'),
                            str(source), str(launcher)], check=True, capture_output=True)
            result = subprocess.run([str(launcher), '--spec', 'path with spaces'],
                env=dict(os.environ, DYLD_LIBRARY_PATH='/fixture/installed-libraries'),
                text=True, capture_output=True, check=True)
            self.assertEqual(json.loads(result.stdout),
                             ['/fixture/installed-libraries', ['--spec', 'path with spaces']])
