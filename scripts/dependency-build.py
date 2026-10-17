#!/usr/bin/env python3
"""Build pinned native dependencies in private workspaces and publish complete caches."""
import argparse
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import shlex
import shutil
import subprocess
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
TARGET = 'aarch64-linux-musl'
PACKAGES = {
    'e2fsprogs': ('1.47.2', '08242e64ca0e8194d9c1caad49762b19209a06318199b63ce74ae4ef2d74e63c', 'https://mirrors.edge.kernel.org/pub/linux/kernel/people/tytso/e2fsprogs/v1.47.2/e2fsprogs-1.47.2.tar.xz'),
    'dhcpcd': ('10.3.0', '06e4c1aaf958523f3fd1c57258c613c6c7ae56b8f1d678fa7943495d5ea6aeb5', 'https://github.com/NetworkConfiguration/dhcpcd/releases/download/v10.3.0/dhcpcd-10.3.0.tar.xz'),
    'vmnet-helper': ('222c121ba31e49856c9ec6c3a14426b49d77c8fe', '1d0e81592837742f8eb74f9b91b2d6b27dd558c71cbc927d42863b71e764404a', 'https://codeload.github.com/nirs/vmnet-helper/tar.gz/222c121ba31e49856c9ec6c3a14426b49d77c8fe'),
}
PATCHES = {'dhcpcd': 'dhcpcd-eloop-retire-closed-fd.patch', 'vmnet-helper': 'vmnet-helper-nat66-prefix.patch'}
ARTIFACTS = {'e2fsprogs': ['misc/mke2fs', 'e2fsck/e2fsck', 'resize/resize2fs', 'debugfs/debugfs'], 'dhcpcd': ['src/dhcpcd', 'LICENSE'], 'vmnet-helper': ['vmnet-helper']}


def run(args, **kwargs):
    subprocess.run(args, check=True, **kwargs)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


@contextlib.contextmanager
def locked(path):
    with path.open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        yield


def download(cache, checksum, url):
    archive = cache / (checksum + '.tar')
    with locked(cache / (checksum + '.lock')):
        if archive.exists() and digest(archive) == checksum:
            return archive
        with tempfile.TemporaryDirectory(prefix='download-', dir=cache) as directory:
            pending = Path(directory) / 'source'
            with urllib.request.urlopen(url) as response, pending.open('wb') as output:
                shutil.copyfileobj(response, output)
            if digest(pending) != checksum:
                raise RuntimeError('source checksum mismatch: ' + url)
            os.replace(pending, archive)
    return archive


def build(name, out):
    native = name == 'vmnet-helper'
    if native and platform.system() != 'Darwin':
        raise RuntimeError('vmnet-helper requires the Mac SDK')
    jobs = int(os.environ.get('BUILD_JOBS', os.cpu_count() or 1))
    if jobs < 1:
        raise ValueError('BUILD_JOBS must be positive')
    cache = Path(os.environ.get(name.upper().replace('-', '_') + '_CACHE', ROOT / 'build' / name)).resolve()
    cache.mkdir(parents=True, exist_ok=True)
    version, checksum, url = PACKAGES[name]
    patch = ROOT / 'scripts' / 'patches' / PATCHES[name] if name in PATCHES else None
    compiler = ['xcrun', 'clang', '--version'] if native else ['zig', 'version']
    compiler_path = shutil.which(compiler[0])
    identity = {
        'source': checksum, 'recipe': digest(Path(__file__)),
        'patch': digest(patch) if patch else None,
        'compiler': [compiler_path, subprocess.check_output(compiler, text=True)],
        'host': [platform.system(), platform.machine()], 'target': 'native' if native else TARGET,
        'flags': {key: os.environ.get(key) for key in ('CFLAGS', 'CPPFLAGS', 'LDFLAGS', 'SDKROOT', 'MACOSX_DEPLOYMENT_TARGET', 'DEVELOPER_DIR')},
    }
    if native:
        identity['sdk'] = subprocess.check_output(['xcrun', '--show-sdk-path'], text=True)
    else:
        identity['build_cc'] = [shutil.which('cc'), subprocess.check_output(['cc', '--version'], text=True)]
    key = hashlib.sha256(json.dumps(identity, sort_keys=True).encode()).hexdigest()
    complete = cache / ('artifacts-' + key)
    with locked(cache / (key + '.lock')):
        if not complete.is_dir():
            archive = download(cache, checksum, url)
            with tempfile.TemporaryDirectory(prefix='work-', dir=cache) as directory:
                work = Path(directory)
                src = work / 'src'
                src.mkdir()
                run(['tar', '-C', str(src), '--strip-components=1', '-xf', str(archive)])
                if patch:
                    with patch.open('rb') as stream:
                        run(['patch', '-s', '-d', str(src), '-p1'], stdin=stream)
                env = os.environ.copy()
                env.pop('MAKEFLAGS', None)
                env.pop('MFLAGS', None)
                if native:
                    (src / 'version.h').write_text('#define GIT_VERSION "v0.13.0-clankerd-prefix1"\n#define GIT_COMMIT "' + version + '"\n')
                    run(['xcrun', 'clang', '-std=gnu99', '-O2', '-fblocks', '-mmacosx-version-min=15.0', '-I', str(src), '-I', str(src / 'programs'), '-I', str(src / 'broker'), *[str(src / p) for p in ('programs/helper.c', 'programs/options.c', 'programs/os.c', 'broker/client.c')], '-framework', 'vmnet', '-framework', 'CoreFoundation', '-o', str(src / 'vmnet-helper')])
                else:
                    for tool, command in {'cc': ['cc', '-target', TARGET], 'ar': ['ar'], 'ranlib': ['ranlib']}.items():
                        wrapper = work / ('clankerd-' + tool)
                        wrapper.write_text('#!/bin/sh\nexec ' + shlex.join([str(compiler_path), *command]) + ' "$@"\n')
                        wrapper.chmod(0o755)
                        env[{'cc': 'CC', 'ar': 'AR', 'ranlib': 'RANLIB'}[tool]] = wrapper.name
                    env['PATH'] = str(work) + os.pathsep + env.get('PATH', '')
                    env['BUILD_CC'] = 'cc'
                    env['LDFLAGS'] = '-static -s'
                    flags = ['--disable-nls', '--disable-fuse2fs', '--disable-uuidd', '--disable-defrag', '--disable-e2initrd-helper'] if name == 'e2fsprogs' else ['--disable-privsep', '--without-dev', '--dbdir=/run/clankerd/dhcpcd', '--rundir=/run/clankerd/dhcpcd']
                    run(['./configure', '--host=' + TARGET, *flags], cwd=src, env=env, stdout=subprocess.DEVNULL)
                    run(['make', '-j' + str(jobs), '-s'], cwd=src, env=env, stdout=subprocess.DEVNULL)
                staged = work / 'artifacts'
                staged.mkdir()
                for artifact in ARTIFACTS[name]:
                    destination = staged / ('dhcpcd.LICENSE' if artifact == 'LICENSE' else Path(artifact).name)
                    shutil.copyfile(src / artifact, destination)
                    destination.chmod(0o644 if artifact == 'LICENSE' else 0o755)
                os.replace(staged, complete)
        out.mkdir(parents=True, exist_ok=True)
        for artifact in complete.iterdir():
            fd, temporary = tempfile.mkstemp(prefix='.' + artifact.name + '-', dir=out)
            os.close(fd)
            try:
                shutil.copy2(artifact, temporary)
                os.replace(temporary, out / artifact.name)
            finally:
                Path(temporary).unlink(missing_ok=True)
    print('installed ' + name + ' into ' + str(out))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('package', choices=PACKAGES)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    build(args.package, args.output.resolve())
