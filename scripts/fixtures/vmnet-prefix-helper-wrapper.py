#!/usr/bin/python3
"""Test-only prefix selection; configuration is adjacent to this copied wrapper."""
import json
import os
from pathlib import Path
import sys


def arguments(args, config):
    def option(name, default=None):
        return args[args.index(name) + 1] if name in args else default
    prefix = config['prefixes'].get(option('--start-address'))
    if prefix and option('--operation-mode', 'shared') == 'shared':
        if '--nat66-prefix' in args:
            raise ValueError('Duplicate prefix setting')
        return [*args, '--nat66-prefix', prefix]
    return args


if __name__ == '__main__':
    config = json.loads(Path(__file__).with_suffix('.json').read_text())
    os.execv(config['helper'], [config['helper'], *arguments(sys.argv[1:], config)])
