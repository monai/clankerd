#!/usr/bin/env python3
"""Run one Make graph recipe in a private directory and publish after success."""
import fcntl
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from importlib.util import module_from_spec, spec_from_file_location

tree_spec = spec_from_file_location('artifact_tree', Path(__file__).with_name('artifact-tree.py'))
tree = module_from_spec(tree_spec)
tree_spec.loader.exec_module(tree)
separator = sys.argv.index('--')
output = Path(sys.argv[1]).absolute()
inputs = [Path(value) for value in sys.argv[2:separator]]
output.parent.mkdir(parents=True, exist_ok=True)
with (output.parent / ('.' + output.name + '.lock')).open('a') as lock:
    fcntl.flock(lock, fcntl.LOCK_EX)
    import json
    try:
        complete = output / '.complete'
        if (all(path.stat().st_mtime_ns <= complete.stat().st_mtime_ns for path in inputs)
                and json.loads(complete.read_text()).get('recipe') == sys.argv[separator+1:]
                and json.loads(complete.read_text()).get('files') == tree.inventory(output)):
            sys.exit(0)
    except (OSError, ValueError):
        pass
    generations = output.parent / ('.' + output.name + '-generations')
    generations.mkdir(exist_ok=True)
    stage = Path(tempfile.mkdtemp(prefix='generation-', dir=generations))
    # Recipes own creation of their root (copytree/extraction need an absent path).
    stage.rmdir()
    link = generations / ('publish-' + str(os.getpid()))
    try:
        command = [arg.replace('{out}', str(stage)) for arg in sys.argv[separator+1:]]
        subprocess.run(command, check=True)
        (stage / '.complete').write_text(json.dumps({'recipe': sys.argv[separator+1:], 'files': tree.inventory(stage)}, sort_keys=True) + '\n')
        link.symlink_to(os.path.relpath(stage, output.parent))
        # Link is renamed into output.parent; its relative target is for there.
        os.replace(link, output)
    except BaseException:
        shutil.rmtree(stage, ignore_errors=True)
        raise
    finally:
        link.unlink(missing_ok=True)
